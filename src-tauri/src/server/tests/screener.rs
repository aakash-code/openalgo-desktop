//! End-to-end checks for the Screener bulk-OHLCV route. Session-authed POST
//! (CSRF token carried), server booted in-process. No history is seeded, so
//! the symbol comes back under `missing` rather than erroring -- which proves
//! the route, service and Historify read are wired, and validation rejects a
//! bad request.

use super::*;

fn body(symbols: Value, interval: &str, bars: i64) -> Value {
    json!({ "symbols": symbols, "interval": interval, "bars": bars })
}

#[tokio::test]
async fn bulk_ohlcv_reports_missing_when_no_history() {
    let h = H::new();
    h.setup();
    let (cookie, csrf) = h.session(true);
    let req = with_session(
        post_json(
            "/historify/api/bulk-ohlcv",
            body(json!([{"symbol": "INFY", "exchange": "NSE"}]), "5m", 50),
        ),
        &cookie,
        Some(&csrf),
    );
    let (s, v) = h.json(req).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], json!("success"), "{v}");
    assert!(
        v["missing"].as_array().unwrap().iter().any(|m| m == "NSE:INFY"),
        "{v}"
    );
    assert!(v["data"].as_object().unwrap().is_empty(), "{v}");
}

#[tokio::test]
async fn bulk_ohlcv_refuses_empty_symbols() {
    let h = H::new();
    h.setup();
    let (cookie, csrf) = h.session(true);
    let req = with_session(
        post_json("/historify/api/bulk-ohlcv", body(json!([]), "5m", 50)),
        &cookie,
        Some(&csrf),
    );
    let (s, _v) = h.json(req).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}
