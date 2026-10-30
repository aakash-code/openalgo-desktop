//! Writer Flow's per-minute OI-flow store (DuckDB). The open interest and
//! premium of every option contract around ATM, sampled through the day, so the
//! analysis can say where writers have been adding, covering or holding.
//!
//! Fully isolated: its own file (`oi_flow.duckdb` beside the other stores),
//! its own schema, nothing shared with the SQLite DBs or historify. Port of the
//! fork's `database/oi_flow_db.py`.
//!
//! Timestamps are stored and read as text, because Desktop's `duckdb` crate is
//! built without the chrono feature: inserts pass `YYYY-MM-DD[ HH:MM:SS]`
//! strings (DuckDB casts them to DATE/TIMESTAMP) and reads select them back
//! through `strftime`, exactly as `historify/db` does.

use crate::db::duckdb::DuckDb;
use crate::error::Result;
use crate::services::oi_flow_analysis::Sample;
use chrono::{NaiveDate, NaiveDateTime};
use duckdb::params;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// The columns of `oi_flow_samples`, in table order.
const SAMPLE_COLUMNS: &[&str] = &[
    "trade_date",
    "sample_time",
    "underlying",
    "exchange",
    "expiry",
    "strike",
    "side",
    "symbol",
    "oi",
    "ltp",
    "volume",
    "spot",
    "lot_size",
    "source",
];

static DB: OnceLock<Arc<DuckDb>> = OnceLock::new();

/// Per-underlying write count for this process. Writer Flow keeps its cached
/// answers until this moves, so a refresh with nothing new costs nothing.
static GENERATIONS: Mutex<Option<HashMap<String, u64>>> = parking_lot::const_mutex(None);

/// Where the store lives: `OI_FLOW_DATABASE_PATH`, else `<data_dir>/oi_flow.duckdb`.
fn db_path(data_dir: &Path) -> PathBuf {
    match std::env::var("OI_FLOW_DATABASE_PATH") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => data_dir.join("oi_flow.duckdb"),
    }
}

/// Open the store once and create the tables (idempotent).
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

/// The open store, if [`init`] has run. Routes return empty rather than error
/// when the recorder has never started, so the panels self-hide.
pub fn handle() -> Option<Arc<DuckDb>> {
    DB.get().cloned()
}

fn create_tables(c: &duckdb::Connection) -> Result<()> {
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS oi_flow_samples (
            trade_date   DATE NOT NULL,
            sample_time  TIMESTAMP NOT NULL,
            underlying   VARCHAR NOT NULL,
            exchange     VARCHAR NOT NULL,
            expiry       VARCHAR NOT NULL,
            strike       DOUBLE NOT NULL,
            side         VARCHAR NOT NULL,
            symbol       VARCHAR NOT NULL,
            oi           BIGINT,
            ltp          DOUBLE,
            volume       BIGINT,
            spot         DOUBLE,
            lot_size     INTEGER,
            source       VARCHAR NOT NULL,
            PRIMARY KEY (symbol, sample_time)
        );
        CREATE TABLE IF NOT EXISTS oi_flow_settings (
            key    VARCHAR PRIMARY KEY,
            value  VARCHAR NOT NULL
        );",
    )?;
    Ok(())
}

/// The write count for an underlying; changes after every stored write.
pub fn generation(underlying: &str) -> u64 {
    GENERATIONS
        .lock()
        .as_ref()
        .and_then(|m| m.get(underlying).copied())
        .unwrap_or(0)
}

fn bump_generations(underlyings: &[String]) {
    let mut guard = GENERATIONS.lock();
    let map = guard.get_or_insert_with(HashMap::new);
    for u in underlyings {
        *map.entry(u.clone()).or_insert(0) += 1;
    }
}

/// One row as the recorder produces it (matches `SAMPLE_COLUMNS`).
#[derive(Clone, Debug)]
pub struct FlowRow {
    pub trade_date: NaiveDate,
    pub sample_time: NaiveDateTime,
    pub underlying: String,
    pub exchange: String,
    pub expiry: String,
    pub strike: f64,
    pub side: String,
    pub symbol: String,
    pub oi: Option<i64>,
    pub ltp: Option<f64>,
    pub volume: Option<i64>,
    pub spot: Option<f64>,
    pub lot_size: Option<i32>,
    pub source: String,
}

/// Store samples; a `(symbol, minute)` already held is left as it was, so a
/// backfill over minutes the live recorder caught, and a restarted recorder
/// repeating a minute, neither duplicate nor overwrite. Returns rows offered.
pub fn insert_samples(rows: &[FlowRow]) -> Result<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    let Some(db) = handle() else { return Ok(0) };
    let placeholders = SAMPLE_COLUMNS.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!(
        "INSERT OR IGNORE INTO oi_flow_samples ({}) VALUES ({})",
        SAMPLE_COLUMNS.join(", "),
        placeholders
    );
    {
        let c = db.conn()?;
        // One transaction, so a reader never sees half a minute.
        c.execute_batch("BEGIN TRANSACTION")?;
        let result = (|| -> Result<()> {
            let mut stmt = c.prepare(&sql)?;
            for r in rows {
                stmt.execute(params![
                    r.trade_date.format("%Y-%m-%d").to_string(),
                    r.sample_time.format("%Y-%m-%d %H:%M:%S").to_string(),
                    r.underlying,
                    r.exchange,
                    r.expiry,
                    r.strike,
                    r.side,
                    r.symbol,
                    r.oi,
                    r.ltp,
                    r.volume,
                    r.spot,
                    r.lot_size,
                    r.source,
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
    }
    // After the commit, so an answer built from what a reader saw is never
    // stamped with a count that includes rows it could not see.
    let underlyings: Vec<String> = {
        let mut set: Vec<String> = rows.iter().map(|r| r.underlying.clone()).collect();
        set.sort();
        set.dedup();
        set
    };
    bump_generations(&underlyings);
    Ok(rows.len())
}

/// Validate an underlying against its shape (`^[A-Z0-9][A-Z0-9&-]{0,19}$`), so
/// a value written into the SQL as a literal (DuckDB plans a literal day filter
/// far better than a bound one) carries no injection risk.
fn valid_underlying(u: &str) -> bool {
    let b = u.as_bytes();
    if b.is_empty() || b.len() > 20 {
        return false;
    }
    let first_ok = b[0].is_ascii_uppercase() || b[0].is_ascii_digit();
    first_ok
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'&' || *c == b'-')
}

/// `^\d{2}[A-Z]{3}\d{2}$`, e.g. `30OCT26`.
fn valid_expiry(e: &str) -> bool {
    let b = e.as_bytes();
    b.len() == 7
        && b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[2].is_ascii_uppercase()
        && b[3].is_ascii_uppercase()
        && b[4].is_ascii_uppercase()
        && b[5].is_ascii_digit()
        && b[6].is_ascii_digit()
}

fn day_filter(underlying: &str, expiry: &str, trade_date: NaiveDate) -> Result<String> {
    if !valid_underlying(underlying) {
        return Err(crate::error::AppError::Validation(format!(
            "Not an underlying: {underlying:?}"
        )));
    }
    if !valid_expiry(expiry) {
        return Err(crate::error::AppError::Validation(format!(
            "Not an expiry: {expiry:?}"
        )));
    }
    Ok(format!(
        "underlying = '{underlying}' AND expiry = '{expiry}' AND trade_date = DATE '{}'",
        trade_date.format("%Y-%m-%d")
    ))
}

/// Every sample for one underlying and expiry on one day, oldest first, shaped
/// for [`crate::services::oi_flow_analysis::analyse`].
pub fn load_day(underlying: &str, expiry: &str, trade_date: NaiveDate) -> Result<Vec<Sample>> {
    let Some(db) = handle() else { return Ok(Vec::new()) };
    let where_ = day_filter(underlying, expiry, trade_date)?;
    db.read(|c| {
        let sql = format!(
            "SELECT strftime(sample_time, '%Y-%m-%dT%H:%M:%S'), strike, side, symbol, \
             oi, ltp, spot, lot_size \
             FROM oi_flow_samples WHERE {where_} ORDER BY sample_time, strike, side"
        );
        let mut stmt = c.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let ts: String = r.get(0)?;
            let Ok(sample_time) = NaiveDateTime::parse_from_str(&ts, "%Y-%m-%dT%H:%M:%S") else {
                continue;
            };
            out.push(Sample {
                sample_time,
                strike: r.get(1)?,
                side: r.get(2)?,
                symbol: r.get(3)?,
                oi: r.get::<_, Option<i64>>(4)?.map(|v| v as f64),
                ltp: r.get::<_, Option<f64>>(5)?,
                spot: r.get::<_, Option<f64>>(6)?,
                lot_size: r.get::<_, Option<i64>>(7)?,
            });
        }
        Ok(out)
    })
}

/// A recorded session: underlying, expiry and day held, newest day first.
#[derive(serde::Serialize)]
pub struct Session {
    pub trade_date: String,
    pub underlying: String,
    pub expiry: String,
    pub minutes: i64,
    pub last_sample: Option<String>,
}

pub fn available_sessions(limit_days: i64) -> Result<Vec<Session>> {
    let Some(db) = handle() else { return Ok(Vec::new()) };
    db.read(|c| {
        let latest: Option<String> = c
            .query_row(
                "SELECT strftime(MAX(trade_date), '%Y-%m-%d') FROM oi_flow_samples",
                [],
                |r| r.get(0),
            )
            .ok()
            .flatten();
        let Some(latest) = latest else { return Ok(Vec::new()) };
        let Ok(latest_date) = NaiveDate::parse_from_str(&latest, "%Y-%m-%d") else {
            return Ok(Vec::new());
        };
        let cutoff = latest_date - chrono::Duration::days(limit_days);
        let sql = format!(
            "SELECT strftime(trade_date, '%Y-%m-%d'), underlying, expiry, \
             COUNT(DISTINCT sample_time) AS minutes, \
             strftime(MAX(sample_time), '%Y-%m-%dT%H:%M:%S') AS last_sample \
             FROM oi_flow_samples WHERE trade_date >= DATE '{}' \
             GROUP BY trade_date, underlying, expiry \
             ORDER BY trade_date DESC, underlying, expiry",
            cutoff.format("%Y-%m-%d")
        );
        let mut stmt = c.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(Session {
                trade_date: r.get(0)?,
                underlying: r.get(1)?,
                expiry: r.get(2)?,
                minutes: r.get(3)?,
                last_sample: r.get::<_, Option<String>>(4)?,
            });
        }
        Ok(out)
    })
}

/// The expiry an underlying was recorded on for a day: the one with most rows.
pub fn expiry_for_day(underlying: &str, trade_date: NaiveDate) -> Result<Option<String>> {
    let Some(db) = handle() else { return Ok(None) };
    if !valid_underlying(underlying) {
        return Err(crate::error::AppError::Validation(format!(
            "Not an underlying: {underlying:?}"
        )));
    }
    db.read(|c| {
        let sql = format!(
            "SELECT expiry FROM oi_flow_samples \
             WHERE underlying = '{underlying}' AND trade_date = DATE '{}' \
             GROUP BY expiry ORDER BY COUNT(*) DESC LIMIT 1",
            trade_date.format("%Y-%m-%d")
        );
        let v: Option<String> = c.query_row(&sql, [], |r| r.get(0)).ok();
        Ok(v)
    })
}

/// A stored switch, or None when it was never set.
pub fn read_setting(ctx: &crate::state::AppState, key: &str) -> Result<Option<String>> {
    // Settings persist independently of the recorder, so open the store on
    // demand (idempotent -- the recorder shares the same handle).
    let db = init(&ctx.data_dir)?;
    db.read(|c| {
        let v: Option<String> = c
            .query_row(
                "SELECT value FROM oi_flow_settings WHERE key = ?",
                params![key],
                |r| r.get(0),
            )
            .ok();
        Ok(v)
    })
}

pub fn write_setting(ctx: &crate::state::AppState, key: &str, value: &str) -> Result<()> {
    let db = init(&ctx.data_dir)?;
    let c = db.conn()?;
    c.execute(
        "INSERT OR REPLACE INTO oi_flow_settings (key, value) VALUES (?, ?)",
        params![key, value],
    )?;
    Ok(())
}
