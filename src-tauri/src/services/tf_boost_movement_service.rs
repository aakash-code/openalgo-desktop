//! Turns the Intraday Boost snapshot history into current-day movement state,
//! and records new snapshots. The movement maths is the pure engine in
//! [`crate::services::tf_rank_movement`] -- this module is the I/O around it:
//! read the day's per-symbol rank and change timelines from the DuckDB store,
//! feed each symbol through the engine, and return the rows newest-rank-first.
//! Port of the fork's `services/tf_rank_movement_service.movement_snapshot` plus
//! the snapshot-query and record paths.

use crate::services::core::Reply;
use crate::services::{tf_boost_db, tf_rank_movement, tradefinder_service};
use crate::services::tf_boost_db::SnapshotItem;
use crate::state::AppState;
use chrono::{FixedOffset, Utc};
use serde_json::{json, Map, Value};
use tracing::warn;

/// The three ranked lists the recorder stores. `sector_index` (the sector
/// rfactor index the fork also stores here) is not recorded by Desktop yet.
const LIST_TYPES: [&str; 3] = ["intraday_boost", "breakout_beacon", "high_powered_stocks"];

fn ist() -> FixedOffset {
    FixedOffset::east_opt(5 * 3600 + 30 * 60).expect("IST offset is valid")
}

/// Today in IST as `YYYY-MM-DD` -- the recorder stamps `snapshot_date` in IST,
/// so "today" for a query must be the IST day too.
fn today_ist() -> String {
    Utc::now().with_timezone(&ist()).format("%Y-%m-%d").to_string()
}

fn ensure(ctx: &AppState) {
    if let Err(e) = tf_boost_db::init(&ctx.data_dir) {
        warn!("tf_boost store could not be opened: {e}");
    }
}

/// Per-symbol current-day movement rows for one list, newest rank first.
/// Degrades to an empty list (never an error) when there are no snapshots.
pub fn movement_snapshot(ctx: &AppState, date: &str, list_type: &str) -> Vec<Value> {
    ensure(ctx);
    let day = if date.is_empty() { today_ist() } else { date.to_string() };
    let per_symbol = tf_boost_db::rank_timeline_fine(&day, list_type).unwrap_or_default();
    // The latest minute anyone was recorded at is what "now" means for this
    // list, so it survives a closed market and a replay of an older date alike.
    let latest_minute = per_symbol
        .values()
        .filter_map(|obs| obs.last().map(|o| o.0))
        .fold(None, |acc: Option<f64>, m| Some(acc.map_or(m, |a| a.max(m))));
    let changes = tf_boost_db::change_timeline(&day, list_type).unwrap_or_default();

    let mut rows: Vec<Value> = Vec::new();
    for (sym, obs) in &per_symbol {
        let ch = changes.get(sym).map(|v| v.as_slice());
        if let Some(row) = tf_rank_movement::compute_symbol_movement(sym, obs, latest_minute, ch) {
            rows.push(row);
        }
    }
    rows.sort_by_key(|r| r.get("current_rank").and_then(Value::as_i64).unwrap_or(i64::MAX));
    rows
}

/// Movement reply for the API. A single `symbol` is not supported yet (the
/// fork's episode view is a later follow-up); the full-list path is what the
/// Intraday Boost panel renders.
pub fn movement_reply(ctx: &AppState, date: &str, list_type: &str) -> Reply {
    let rows = movement_snapshot(ctx, date, list_type);
    Reply::ok(json!({
        "status": "success",
        "list_type": list_type,
        "count": rows.len(),
        "symbols": rows,
    }))
}

/// Snapshot query: the day's recorded symbols for a list, with optional rank and
/// price timelines (each keyed by symbol then by day, one day deep). Backs the
/// "how has this symbol's rank/price moved today" view.
pub fn snapshots_reply(
    ctx: &AppState,
    date: &str,
    list_type: &str,
    include_ranks: bool,
    include_prices: bool,
) -> Reply {
    ensure(ctx);
    let day = if date.is_empty() { today_ist() } else { date.to_string() };

    let ranks = tf_boost_db::rank_timeline_fine(&day, list_type).unwrap_or_default();
    // Symbols present today, from whichever timeline we have.
    let mut symbols: Vec<String> = ranks.keys().cloned().collect();
    symbols.sort();

    let nest = |m: &std::collections::HashMap<String, Vec<(f64, f64)>>| -> Value {
        let mut obj = Map::new();
        for (sym, series) in m {
            let pairs: Vec<Value> = series.iter().map(|(x, y)| json!([x, y])).collect();
            let mut by_day = Map::new();
            by_day.insert(day.clone(), Value::Array(pairs));
            obj.insert(sym.clone(), Value::Object(by_day));
        }
        Value::Object(obj)
    };

    let mut out = Map::new();
    out.insert("status".into(), json!("success"));
    out.insert("date".into(), json!(day));
    out.insert("list_type".into(), json!(list_type));
    out.insert("count".into(), json!(symbols.len()));
    out.insert("symbols".into(), json!(symbols));
    if include_ranks {
        // rank is an integer series; reuse the same nester via f64 pairs.
        let as_f64: std::collections::HashMap<String, Vec<(f64, f64)>> = ranks
            .iter()
            .map(|(s, v)| (s.clone(), v.iter().map(|(x, r)| (*x, *r as f64)).collect()))
            .collect();
        out.insert("ranks".into(), nest(&as_f64));
    }
    if include_prices {
        let prices = tf_boost_db::price_timeline(&day, list_type).unwrap_or_default();
        out.insert("prices".into(), nest(&prices));
    }
    Reply::ok(Value::Object(out))
}

/// Fetch the current ranked lists from TradeFinder and store one snapshot per
/// list at `now` (IST). The data producer behind the periodic recorder; it needs
/// a live TradeFinder session, so it returns 0 stored when no token is set. Not
/// run in tests -- those seed the store directly through [`tf_boost_db`].
pub async fn record_now(ctx: &AppState) -> usize {
    ensure(ctx);
    let Some(pulse) = tradefinder_service::fetch_market_pulse(ctx).await else {
        return 0;
    };
    let now = Utc::now().with_timezone(&ist()).naive_local();
    let mut stored = 0;
    for list_type in LIST_TYPES {
        let Some(items) = pulse.get(list_type).and_then(Value::as_array) else {
            continue;
        };
        let rows: Vec<SnapshotItem> = items
            .iter()
            .filter_map(|it| {
                let symbol = it.get("symbol").and_then(Value::as_str)?.to_string();
                if symbol.is_empty() {
                    return None;
                }
                Some(SnapshotItem {
                    symbol,
                    ltp: it.get("ltp").and_then(Value::as_f64),
                    prev_close: it.get("prev_close").and_then(Value::as_f64),
                    change_pct: it.get("change_pct").and_then(Value::as_f64),
                    score: it.get("score").and_then(Value::as_f64),
                })
            })
            .collect();
        match tf_boost_db::insert_snapshot(now, list_type, &rows) {
            Ok(n) => stored += n,
            Err(e) => warn!("tf_boost snapshot write failed for {list_type}: {e}"),
        }
    }
    stored
}
