//! End-to-end checks for the Signal distribution API: the producer ingests a
//! signal (apikey in the body), a retry of the same eventId is idempotent, the
//! cursor backfill reads it back, and a wrong key is refused. Boots the real
//! server in-process; no broker or network needed.

use super::*;

fn signal(event_id: &str, key: &str) -> Value {
    json!({
        "apikey": key,
        "eventId": event_id,
        "event": "open",
        "symbol": "INFY",
        "exchange": "NSE",
        "sector": "IT",
        "side": "LONG",
        "entry": 1500.0,
        "stop": 1485.0,
        "target": 1530.0,
        "stopPct": 1.0,
        "signalTimeIst": "2026-10-30T10:15:00",
    })
}

#[tokio::test]
async fn ingest_stores_and_events_reads_it_back() {
    let h = H::new();
    let key = h.setup();

    let (s, v) = h.json(post_json("/signals/v1/ingest", signal("ev-1", &key))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], json!("success"), "{v}");
    assert!(v["id"].as_i64().is_some(), "{v}");

    // A retry of the same eventId is idempotent: success, no new row.
    let (s2, v2) = h.json(post_json("/signals/v1/ingest", signal("ev-1", &key))).await;
    assert_eq!(s2, StatusCode::OK, "{v2}");
    assert_eq!(v2["duplicate"], json!(true), "{v2}");

    let (s3, v3) = h
        .json(get(&format!("/signals/v1/events?apikey={key}&since_id=0")))
        .await;
    assert_eq!(s3, StatusCode::OK, "{v3}");
    assert_eq!(v3["status"], json!("success"), "{v3}");
    let events = v3["events"].as_array().expect("events array");
    assert!(
        events
            .iter()
            .any(|e| e["eventId"] == "ev-1" && e["symbol"] == "INFY" && e["side"] == "LONG"),
        "{v3}"
    );
}

#[tokio::test]
async fn ingest_refuses_a_wrong_apikey() {
    let h = H::new();
    h.setup();
    let (s, _v) = h
        .json(post_json("/signals/v1/ingest", signal("ev-x", "wrong-key")))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}
