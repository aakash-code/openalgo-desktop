//! End-to-end checks for the TradeFinder Boost movement + snapshot routes. The
//! live recorder needs a TradeFinder session, so instead of fetching, these seed
//! the snapshot store directly with a known rank trajectory and assert that the
//! pure movement engine, driven through the HTTP route, reconstructs it: a stock
//! that climbs from rank 2 to rank 1 reports current_rank 1, first_seen_rank 2,
//! best_rank 1, inside Top-5 and present. Each test uses a unique date so the
//! process-shared store never crosses them. Boots the real server in-process.

use super::*;
use crate::services::tf_boost_db::{self, SnapshotItem};
use chrono::NaiveDate;

fn item(symbol: &str, ltp: f64, change: f64) -> SnapshotItem {
    SnapshotItem {
        symbol: symbol.to_string(),
        ltp: Some(ltp),
        prev_close: Some(100.0),
        change_pct: Some(change),
        score: Some(change),
    }
}

/// Seed one ranked list (items in rank order) at `date` HH:MM on the shared store.
fn seed(ctx: &crate::state::AppState, date: &str, hh: u32, mm: u32, items: &[SnapshotItem]) {
    tf_boost_db::init(&ctx.data_dir).expect("open store");
    let d = NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap();
    let ts = d.and_hms_opt(hh, mm, 0).unwrap();
    tf_boost_db::insert_snapshot(ts, "intraday_boost", items).expect("seed snapshot");
}

#[tokio::test]
async fn boostmovement_reconstructs_a_climb_from_the_snapshots() {
    let h = H::new();
    let key = h.setup();
    let date = "2099-01-05";

    // AAA climbs 2 -> 2 -> 1 -> 1; BBB slips 1 -> 1 -> 2 -> 2. Rank is list order.
    seed(h.ctx(), date, 9, 15, &[item("BBB", 100.0, 0.5), item("AAA", 100.0, 1.0)]);
    seed(h.ctx(), date, 9, 17, &[item("BBB", 101.0, 0.6), item("AAA", 101.0, 1.5)]);
    seed(h.ctx(), date, 9, 19, &[item("AAA", 102.0, 2.0), item("BBB", 100.5, 0.4)]);
    seed(h.ctx(), date, 9, 21, &[item("AAA", 103.0, 2.5), item("BBB", 100.0, 0.2)]);

    let (s, v) = h
        .json(post_json(
            "/api/v1/boostmovement",
            json!({"apikey": key, "date": date, "list_type": "intraday_boost"}),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], json!("success"), "{v}");
    assert_eq!(v["count"], json!(2), "{v}");

    let rows = v["symbols"].as_array().expect("symbols array");
    // Newest rank first: AAA (now rank 1) leads BBB (now rank 2).
    assert_eq!(rows[0]["symbol"], json!("AAA"), "{v}");
    assert_eq!(rows[0]["current_rank"], json!(1), "{v}");
    assert_eq!(rows[0]["first_seen_rank"], json!(2), "{v}");
    assert_eq!(rows[0]["best_rank"], json!(1), "{v}");
    assert_eq!(rows[0]["top5"], json!(true), "{v}");
    assert_eq!(rows[0]["present"], json!(true), "{v}");
    assert_eq!(rows[1]["symbol"], json!("BBB"), "{v}");
    assert_eq!(rows[1]["current_rank"], json!(2), "{v}");
}

#[tokio::test]
async fn boostmovement_with_no_snapshots_is_an_empty_list_not_an_error() {
    let h = H::new();
    let key = h.setup();
    // A date nothing was seeded for.
    let (s, v) = h
        .json(post_json(
            "/api/v1/boostmovement",
            json!({"apikey": key, "date": "2099-12-31", "list_type": "intraday_boost"}),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], json!("success"), "{v}");
    assert_eq!(v["count"], json!(0), "{v}");
    assert_eq!(v["symbols"], json!([]), "{v}");
}

#[tokio::test]
async fn boostsnapshots_lists_recorded_symbols_with_rank_timeline() {
    let h = H::new();
    let key = h.setup();
    let date = "2099-02-10";

    seed(h.ctx(), date, 9, 15, &[item("ZZZ", 50.0, 1.0), item("YYY", 70.0, 0.5)]);
    seed(h.ctx(), date, 9, 16, &[item("ZZZ", 51.0, 1.2), item("YYY", 70.5, 0.6)]);

    let (s, v) = h
        .json(post_json(
            "/api/v1/boostsnapshots",
            json!({"apikey": key, "date": date, "list_type": "intraday_boost", "includeRanks": true}),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], json!("success"), "{v}");
    assert_eq!(v["count"], json!(2), "{v}");
    let symbols = v["symbols"].as_array().expect("symbols");
    assert!(symbols.contains(&json!("YYY")) && symbols.contains(&json!("ZZZ")), "{v}");
    // ranks keyed by symbol then by day; ZZZ has two points.
    let zzz = &v["ranks"]["ZZZ"][date];
    assert_eq!(zzz.as_array().map(|a| a.len()), Some(2), "{v}");
}

#[tokio::test]
async fn boostmovement_refuses_a_wrong_apikey() {
    let h = H::new();
    h.setup();
    let (s, _v) = h
        .json(post_json("/api/v1/boostmovement", json!({"apikey": "wrong-key"})))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}
