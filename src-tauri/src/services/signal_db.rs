//! Signal distribution store (main SQLite DB). One table, `signal_events`:
//! an autoincrement `id` that doubles as the backfill cursor and a UNIQUE
//! `event_id` for idempotency, so a producer retry neither duplicates nor
//! errors. Port of the fork's `database/signal_db.py` (events half; the
//! subscriber/webhook tables are a later stage).

use crate::error::Result;
use crate::state::AppState;
use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};

fn ensure_table(c: &Connection) -> Result<()> {
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS signal_events (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            event_id        TEXT NOT NULL UNIQUE,
            event           TEXT NOT NULL,
            symbol          TEXT NOT NULL,
            exchange        TEXT NOT NULL,
            sector          TEXT NOT NULL,
            side            TEXT NOT NULL,
            entry           REAL NOT NULL,
            stop            REAL NOT NULL,
            target          REAL NOT NULL,
            stop_pct        REAL NOT NULL,
            signal_time_ist TEXT NOT NULL,
            created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
        );",
    )?;
    Ok(())
}

fn f(p: &Map<String, Value>, k: &str) -> f64 {
    p.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}
fn s<'a>(p: &'a Map<String, Value>, k: &str) -> &'a str {
    p.get(k).and_then(Value::as_str).unwrap_or_default()
}

/// Insert one event. Returns its new id, or None when `event_id` already
/// existed (the expected path for a producer retry, not an error).
pub fn insert_signal_event(ctx: &AppState, p: &Map<String, Value>) -> Result<Option<i64>> {
    let c = ctx.sqlite.conn()?;
    ensure_table(&c)?;
    let n = c.execute(
        "INSERT OR IGNORE INTO signal_events
           (event_id, event, symbol, exchange, sector, side, entry, stop, target, stop_pct, signal_time_ist)
         VALUES (?,?,?,?,?,?,?,?,?,?,?)",
        params![
            s(p, "eventId"),
            s(p, "event"),
            s(p, "symbol"),
            s(p, "exchange"),
            s(p, "sector"),
            s(p, "side"),
            f(p, "entry"),
            f(p, "stop"),
            f(p, "target"),
            f(p, "stopPct"),
            s(p, "signalTimeIst"),
        ],
    )?;
    Ok(if n == 0 { None } else { Some(c.last_insert_rowid()) })
}

fn row_json(
    id: i64,
    event_id: String,
    event: String,
    symbol: String,
    exchange: String,
    sector: String,
    side: String,
    entry: f64,
    stop: f64,
    target: f64,
    stop_pct: f64,
    signal_time_ist: String,
    created_at: String,
) -> Value {
    json!({
        "id": id,
        "eventId": event_id,
        "event": event,
        "symbol": symbol,
        "exchange": exchange,
        "sector": sector,
        "side": side,
        "entry": entry,
        "stop": stop,
        "target": target,
        "stopPct": stop_pct,
        "signalTimeIst": signal_time_ist,
        "createdAt": created_at,
    })
}

/// Events with `id` greater than `since_id`, oldest first, for cursor backfill.
pub fn get_events_since(ctx: &AppState, since_id: i64, limit: i64) -> Result<Vec<Value>> {
    let c = ctx.sqlite.conn()?;
    ensure_table(&c)?;
    let mut stmt = c.prepare(
        "SELECT id, event_id, event, symbol, exchange, sector, side, entry, stop, target,
                stop_pct, signal_time_ist, created_at
         FROM signal_events WHERE id > ?1 ORDER BY id ASC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![since_id, limit.clamp(1, 1000)], |r| {
        Ok(row_json(
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
            r.get(7)?,
            r.get(8)?,
            r.get(9)?,
            r.get(10)?,
            r.get(11)?,
            r.get(12)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}
