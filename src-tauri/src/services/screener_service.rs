//! Screener bar delivery: the most recent candles for many symbols at once,
//! read from the Historify DuckDB and returned as columnar arrays keyed
//! "EXCHANGE:SYMBOL". It does NOT evaluate indicators or filters -- the scan
//! runs in the browser worker so a screener value equals the chart value by
//! construction. Port of the fork's `services/screener_service.py` (stored
//! bars; the live forming-candle stitch is a follow-up that needs a quote
//! batch and so an apikey).

use crate::historify::db;
use crate::state::AppState;
use serde_json::{json, Map, Value};

const MAX_SYMBOLS: usize = 500;
const MIN_BARS: i64 = 50;
const MAX_BARS: i64 = 1000;
pub const DEFAULT_BARS: i64 = 300;
const SUPPORTED: &[&str] = &[
    "1m", "3m", "5m", "10m", "15m", "30m", "1h", "D", "W", "M",
];

/// Bars for a screener scan: `(success, payload, http_status)`. The payload's
/// `data` maps "EXCHANGE:SYMBOL" to columnar arrays (t/o/h/l/c/v); `missing`
/// names the symbols Historify holds nothing for.
pub fn scan_bars(
    ctx: &AppState,
    pairs: &[(String, String)],
    interval: &str,
    bars: i64,
) -> (bool, Value, u16) {
    if !SUPPORTED.contains(&interval) {
        return (
            false,
            json!({"status": "error", "message": format!("Unsupported interval '{interval}'. Supported: {}", SUPPORTED.join(", "))}),
            400,
        );
    }
    if pairs.is_empty() {
        return (false, json!({"status": "error", "message": "No symbols provided"}), 400);
    }
    if pairs.len() > MAX_SYMBOLS {
        return (
            false,
            json!({"status": "error", "message": format!("{} symbols requested, limit is {MAX_SYMBOLS}", pairs.len())}),
            400,
        );
    }
    if !(MIN_BARS..=MAX_BARS).contains(&bars) {
        return (
            false,
            json!({"status": "error", "message": format!("bars must be {MIN_BARS}-{MAX_BARS}")}),
            400,
        );
    }

    let read = ctx.duckdb.read(|c| {
        let mut data = Map::new();
        let mut missing = Vec::new();
        for (symbol, exchange) in pairs {
            let all = db::ohlcv(c, symbol, exchange, interval, None, None)?;
            let key = format!("{}:{}", exchange.to_uppercase(), symbol.to_uppercase());
            if all.is_empty() {
                missing.push(key);
                continue;
            }
            let from = all.len().saturating_sub(bars as usize);
            let slice = &all[from..];
            data.insert(
                key,
                json!({
                    "t": slice.iter().map(|b| b.timestamp).collect::<Vec<_>>(),
                    "o": slice.iter().map(|b| b.open).collect::<Vec<_>>(),
                    "h": slice.iter().map(|b| b.high).collect::<Vec<_>>(),
                    "l": slice.iter().map(|b| b.low).collect::<Vec<_>>(),
                    "c": slice.iter().map(|b| b.close).collect::<Vec<_>>(),
                    "v": slice.iter().map(|b| b.volume).collect::<Vec<_>>(),
                }),
            );
        }
        Ok((data, missing))
    });

    match read {
        Ok((data, missing)) => (
            true,
            json!({
                "status": "success",
                "interval": interval,
                "bars": bars,
                "live": false,
                "data": data,
                "missing": missing,
            }),
            200,
        ),
        Err(e) => {
            tracing::error!("screener scan failed: {e}");
            (false, json!({"status": "error", "message": "Could not read history."}), 500)
        }
    }
}
