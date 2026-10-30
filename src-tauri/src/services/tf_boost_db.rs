//! TradeFinder Intraday Boost snapshot store (DuckDB). The recorder stamps the
//! ranked lists once a minute (twice, in practice, so a badge reaches the screen
//! inside 40s), and this store lets the movement engine reconstruct the whole
//! path a stock took to its rank -- where it is, where it has been, how fast it
//! is moving -- from the current day's snapshots alone, with no upstream fetch.
//!
//! Port of the fork's `database/tf_boost_db.py` (core columns only; the
//! per-symbol enrichment columns the fork adds -- CPR, first-candle, directional,
//! momentum -- are a later follow-up and are not written here).
//!
//! Fully isolated: its own `tf_boost_snapshots.duckdb` beside the other stores.
//! Timestamps are stored and read as text, because Desktop's `duckdb` crate is
//! built without the chrono feature: inserts pass `YYYY-MM-DD[ HH:MM:SS]` strings
//! cast to DATE/TIMESTAMP, and reads derive the minute-of-day through DuckDB's
//! `hour`/`minute`/`second`, exactly as the fork's SQL does.

use crate::db::duckdb::DuckDb;
use crate::error::Result;
use chrono::NaiveDateTime;
use duckdb::params;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

static DB: OnceLock<Arc<DuckDb>> = OnceLock::new();

/// One ranked-list row to store. `rank` is assigned from list position by
/// [`insert_snapshot`], so the caller passes items already in ranked order.
#[derive(Clone, Debug)]
pub struct SnapshotItem {
    pub symbol: String,
    pub ltp: Option<f64>,
    pub prev_close: Option<f64>,
    pub change_pct: Option<f64>,
    pub score: Option<f64>,
}

/// Where the store lives: `TF_BOOST_DATABASE_PATH`, else
/// `<data_dir>/tf_boost_snapshots.duckdb`.
fn db_path(data_dir: &Path) -> PathBuf {
    match std::env::var("TF_BOOST_DATABASE_PATH") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => data_dir.join("tf_boost_snapshots.duckdb"),
    }
}

/// Open the store once and create the table (idempotent).
pub fn init(data_dir: &Path) -> Result<Arc<DuckDb>> {
    if let Some(db) = DB.get() {
        return Ok(db.clone());
    }
    let db = Arc::new(DuckDb::new(&db_path(data_dir))?);
    {
        let c = db.conn()?;
        create_tables(&c)?;
    }
    // A race loser takes the winner's handle; only one file is ever opened.
    let _ = DB.set(db);
    Ok(DB.get().unwrap().clone())
}

/// The open store, if [`init`] has run. Readers return empty rather than error
/// when the recorder has never started, so the panels self-hide.
pub fn handle() -> Option<Arc<DuckDb>> {
    DB.get().cloned()
}

fn create_tables(c: &duckdb::Connection) -> Result<()> {
    c.execute_batch(
        "CREATE SEQUENCE IF NOT EXISTS tf_boost_snapshots_id_seq START 1;
         CREATE TABLE IF NOT EXISTS tf_boost_snapshots (
            id             BIGINT PRIMARY KEY DEFAULT nextval('tf_boost_snapshots_id_seq'),
            snapshot_date  DATE NOT NULL,
            snapshot_time  TIMESTAMP NOT NULL,
            list_type      VARCHAR NOT NULL,
            rank           INTEGER NOT NULL,
            symbol         VARCHAR NOT NULL,
            ltp            DOUBLE,
            prev_close     DOUBLE,
            change_pct     DOUBLE,
            score          DOUBLE,
            created_at     TIMESTAMP DEFAULT current_timestamp,
            UNIQUE (snapshot_time, list_type, symbol)
         );",
    )?;
    Ok(())
}

/// Store one ranked list at `snapshot_time`. Rank is the 1-based list position.
/// A repeat of the same (time, list_type, symbol) is ignored, so the recorder's
/// twice-a-minute sampling and any retry are both idempotent. Returns 0 when the
/// store was never opened.
pub fn insert_snapshot(
    snapshot_time: NaiveDateTime,
    list_type: &str,
    items: &[SnapshotItem],
) -> Result<usize> {
    if items.is_empty() {
        return Ok(0);
    }
    let Some(db) = handle() else { return Ok(0) };
    let date_s = snapshot_time.format("%Y-%m-%d").to_string();
    let time_s = snapshot_time.format("%Y-%m-%d %H:%M:%S").to_string();
    let sql = "INSERT OR IGNORE INTO tf_boost_snapshots \
        (snapshot_date, snapshot_time, list_type, rank, symbol, ltp, prev_close, change_pct, score) \
        VALUES (CAST(? AS DATE), CAST(? AS TIMESTAMP), ?, ?, ?, ?, ?, ?, ?)";
    let c = db.conn()?;
    c.execute_batch("BEGIN TRANSACTION")?;
    let result = (|| -> Result<()> {
        let mut stmt = c.prepare(sql)?;
        for (i, it) in items.iter().enumerate() {
            stmt.execute(params![
                date_s,
                time_s,
                list_type,
                (i as i32) + 1,
                it.symbol,
                it.ltp,
                it.prev_close,
                it.change_pct,
                it.score,
            ])?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => c.execute_batch("COMMIT")?,
        Err(e) => {
            let _ = c.execute_batch("ROLLBACK");
            return Err(e);
        }
    }
    Ok(items.len())
}

/// Validate a `YYYY-MM-DD` day string, so it can be bound as a literal date with
/// no risk. Returns the string back when well-formed.
fn valid_day(day: &str) -> bool {
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").is_ok()
}

/// `{symbol: [(minute_of_day_float, rank), ...]}` for one day at the recorder's
/// real resolution (minute-of-day as a FLOAT, so 604.5 is 10:04:30 and a
/// velocity still reads per minute). Keyed by whole minutes the second sample of
/// each minute would collide with the first and be dropped, throwing half the
/// record away -- hence the fractional minute.
pub fn rank_timeline_fine(day: &str, list_type: &str) -> Result<HashMap<String, Vec<(f64, i64)>>> {
    let Some(db) = handle() else { return Ok(HashMap::new()) };
    if !valid_day(day) {
        return Ok(HashMap::new());
    }
    db.read(|c| {
        let sql = "SELECT symbol, \
             hour(snapshot_time) * 60 + minute(snapshot_time) + second(snapshot_time) / 60.0 AS mod, \
             rank \
             FROM tf_boost_snapshots \
             WHERE snapshot_date = CAST(? AS DATE) AND list_type = ? \
             ORDER BY symbol, snapshot_time";
        let mut stmt = c.prepare(sql)?;
        let mut rows = stmt.query(params![day, list_type])?;
        let mut out: HashMap<String, Vec<(f64, i64)>> = HashMap::new();
        while let Some(r) = rows.next()? {
            let sym: String = r.get(0)?;
            let mod_: f64 = r.get(1)?;
            let rank: i32 = r.get(2)?;
            out.entry(sym).or_default().push((mod_, rank as i64));
        }
        Ok(out)
    })
}

/// `{symbol: [(minute_of_day_float, change_pct), ...]}` for one day. Percent, not
/// ltp, because the directional-run engine compares a move against its own
/// pullback and points of percent are comparable across price levels. Rows whose
/// `prev_close` was later corrected are dropped (they were computed against a
/// reference the day's final value disagrees with); a NULL change_pct is skipped.
pub fn change_timeline(day: &str, list_type: &str) -> Result<HashMap<String, Vec<(f64, f64)>>> {
    let Some(db) = handle() else { return Ok(HashMap::new()) };
    if !valid_day(day) {
        return Ok(HashMap::new());
    }
    db.read(|c| {
        let sql = "WITH settled AS ( \
             SELECT symbol, snapshot_date, arg_max(prev_close, snapshot_time) AS final_prev_close \
             FROM tf_boost_snapshots \
             WHERE snapshot_date = CAST(? AS DATE) AND list_type = ? AND prev_close IS NOT NULL \
             GROUP BY symbol, snapshot_date ) \
             SELECT s.symbol, \
                 hour(s.snapshot_time) * 60 + minute(s.snapshot_time) + second(s.snapshot_time) / 60.0 AS mod, \
                 s.change_pct \
             FROM tf_boost_snapshots s \
             LEFT JOIN settled f ON f.symbol = s.symbol AND f.snapshot_date = s.snapshot_date \
             WHERE s.snapshot_date = CAST(? AS DATE) AND s.list_type = ? AND s.change_pct IS NOT NULL \
               AND (f.final_prev_close IS NULL OR s.prev_close IS NULL OR s.prev_close = f.final_prev_close) \
             ORDER BY s.symbol, s.snapshot_time";
        let mut stmt = c.prepare(sql)?;
        let mut rows = stmt.query(params![day, list_type, day, list_type])?;
        let mut out: HashMap<String, Vec<(f64, f64)>> = HashMap::new();
        while let Some(r) = rows.next()? {
            let sym: String = r.get(0)?;
            let mod_: f64 = r.get(1)?;
            let change: f64 = r.get(2)?;
            out.entry(sym).or_default().push((mod_, change));
        }
        Ok(out)
    })
}

/// `{symbol: [(minute_of_day_float, ltp), ...]}` for one day -- the price
/// sparkline behind each rank row.
pub fn price_timeline(day: &str, list_type: &str) -> Result<HashMap<String, Vec<(f64, f64)>>> {
    let Some(db) = handle() else { return Ok(HashMap::new()) };
    if !valid_day(day) {
        return Ok(HashMap::new());
    }
    db.read(|c| {
        let sql = "SELECT symbol, \
             hour(snapshot_time) * 60 + minute(snapshot_time) + second(snapshot_time) / 60.0 AS mod, \
             ltp \
             FROM tf_boost_snapshots \
             WHERE snapshot_date = CAST(? AS DATE) AND list_type = ? AND ltp IS NOT NULL \
             ORDER BY symbol, snapshot_time";
        let mut stmt = c.prepare(sql)?;
        let mut rows = stmt.query(params![day, list_type])?;
        let mut out: HashMap<String, Vec<(f64, f64)>> = HashMap::new();
        while let Some(r) = rows.next()? {
            let sym: String = r.get(0)?;
            let mod_: f64 = r.get(1)?;
            let ltp: f64 = r.get(2)?;
            out.entry(sym).or_default().push((mod_, ltp));
        }
        Ok(out)
    })
}
