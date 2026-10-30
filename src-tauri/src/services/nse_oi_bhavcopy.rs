//! NSE F&O bhavcopy: the previous session's closing open interest for the whole
//! market in one ~1 MB zipped CSV, so OI Profile's "Change in OI" can anchor on
//! a published close instead of 80-odd per-leg broker-history calls. Port of the
//! fork's `services/nse_oi_bhavcopy.py`.
//!
//! The parse (`parse_bhavcopy`, `option_symbol`) is pure and unit-tested. The
//! download walks back from the latest weekday until NSE serves a file (a 404 is
//! a non-trading day, not an outage); that path is network and needs live
//! testing. NSE-only -- BSE/crypto keep the per-leg previous_day_oi anchor.

use crate::state::AppState;
use chrono::{Datelike, Duration, NaiveDate, Utc, Weekday};
use chrono_tz::Asia::Kolkata;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};
use std::sync::Arc;

const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];
const URL_PREFIX: &str = "https://nsearchives.nseindia.com/content/fo/BhavCopy_NSE_FO_0_0_0_";
const URL_SUFFIX: &str = "_F_0000.csv.zip";

/// The previous session's closing OI, keyed by OpenAlgo option symbol, and the
/// underlyings fully covered (every one of their rows parsed).
pub struct Bhavcopy {
    pub oi: HashMap<String, f64>,
    pub underlyings: HashSet<String>,
    pub day: NaiveDate,
}

/// `[Base][DDMMMYY][Strike][CE/PE]`, the strike whole-bare or fractional (292.5),
/// the way the symbol master writes it.
pub fn option_symbol(ticker: &str, expiry: &str, strike: &str, opt: &str) -> Option<String> {
    let d = NaiveDate::parse_from_str(expiry.trim(), "%Y-%m-%d").ok()?;
    let expiry_part = format!(
        "{:02}{}{:02}",
        d.day(),
        MONTHS[(d.month() - 1) as usize],
        d.year() % 100
    );
    let v: f64 = strike.trim().parse().ok()?;
    let strike_part = if v == v.trunc() {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    };
    Some(format!("{ticker}{expiry_part}{strike_part}{opt}"))
}

/// Read the zipped UDiFF CSV into {symbol: closing OI} + the names covered. A
/// ticker with any unreadable OI row is dropped from the covered set, so it is
/// never anchored at zero while looking covered.
pub fn parse_bhavcopy(raw: &[u8]) -> Result<(HashMap<String, f64>, HashSet<String>), String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(raw)).map_err(|e| e.to_string())?;
    let csv_idx = (0..archive.len()).find(|&i| {
        archive
            .by_index(i)
            .map(|f| f.name().to_lowercase().ends_with(".csv"))
            .unwrap_or(false)
    });
    let Some(idx) = csv_idx else {
        return Err("bhavcopy archive holds no CSV".into());
    };
    let mut text = String::new();
    archive
        .by_index(idx)
        .map_err(|e| e.to_string())?
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;

    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().ok_or("empty csv")?.split(',').map(str::trim).collect();
    let col = |name: &str| header.iter().position(|h| *h == name);
    let (Some(c_opt), Some(c_tkr), Some(c_exp), Some(c_strk), Some(c_oi)) = (
        col("OptnTp"),
        col("TckrSymb"),
        col("XpryDt"),
        col("StrkPric"),
        col("OpnIntrst"),
    ) else {
        return Err("bhavcopy CSV missing expected columns".into());
    };

    let mut oi = HashMap::new();
    let mut underlyings = HashSet::new();
    let mut unreadable = HashSet::new();
    for line in lines {
        let f: Vec<&str> = line.split(',').collect();
        let get = |i: usize| f.get(i).map(|s| s.trim()).unwrap_or("");
        let opt = get(c_opt);
        if opt != "CE" && opt != "PE" {
            continue;
        }
        let ticker = get(c_tkr);
        let Some(symbol) = option_symbol(ticker, get(c_exp), get(c_strk), opt) else {
            continue;
        };
        underlyings.insert(ticker.to_string());
        match get(c_oi).parse::<f64>() {
            Ok(v) => {
                oi.insert(symbol, v);
            }
            Err(_) => {
                unreadable.insert(ticker.to_string());
            }
        }
    }
    for u in &unreadable {
        underlyings.remove(u);
    }
    Ok((oi, underlyings))
}

/// `TATAMOTORS30OCT2625000CE` -> `TATAMOTORS`: the base before the expiry.
pub fn underlying_of(symbol: &str) -> Option<String> {
    // Expiry starts at the first two digits followed by a month code.
    let b = symbol.as_bytes();
    for i in 0..b.len().saturating_sub(4) {
        if b[i].is_ascii_digit() && b[i + 1].is_ascii_digit() {
            let mon = &symbol[i + 2..(i + 5).min(symbol.len())];
            if MONTHS.contains(&mon.to_uppercase().as_str()) && i > 0 {
                return Some(symbol[..i].to_string());
            }
        }
    }
    None
}

static CACHE: Mutex<Option<Arc<Bhavcopy>>> = parking_lot::const_mutex(None);

fn today_ist() -> NaiveDate {
    Utc::now().with_timezone(&Kolkata).date_naive()
}

fn headers() -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, REFERER, USER_AGENT};
    let mut h = HeaderMap::new();
    h.insert(
        USER_AGENT,
        HeaderValue::from_static(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/120.0 Safari/537.36",
        ),
    );
    h.insert(ACCEPT, HeaderValue::from_static("*/*"));
    h.insert(REFERER, HeaderValue::from_static("https://www.nseindia.com/"));
    h
}

/// The newest session on or before yesterday whose bhavcopy NSE serves, parsed
/// and cached for the IST day. None when none of the recent days could be read.
pub async fn cached_previous_session_oi(ctx: &AppState) -> Option<Arc<Bhavcopy>> {
    let today = today_ist();
    if let Some(c) = CACHE.lock().as_ref() {
        if c.day >= today - Duration::days(7) {
            return Some(c.clone());
        }
    }
    // Walk back from yesterday over weekdays until one file parses.
    let mut day = today - Duration::days(1);
    for _ in 0..7 {
        if !matches!(day.weekday(), Weekday::Sat | Weekday::Sun) {
            if let Some(bytes) = download_day(ctx, day).await {
                if let Ok((oi, underlyings)) = parse_bhavcopy(&bytes) {
                    if !oi.is_empty() {
                        let bc = Arc::new(Bhavcopy { oi, underlyings, day });
                        *CACHE.lock() = Some(bc.clone());
                        return Some(bc);
                    }
                }
            }
        }
        day -= Duration::days(1);
    }
    None
}

/// The file for one date, or None when NSE published none (404) or did not
/// answer. Network path -- needs live testing.
async fn download_day(ctx: &AppState, day: NaiveDate) -> Option<Vec<u8>> {
    let url = format!("{URL_PREFIX}{}{URL_SUFFIX}", day.format("%Y%m%d"));
    let resp = ctx
        .http
        .get(&url)
        .headers(headers())
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .ok()?;
    if resp.status() != reqwest::StatusCode::OK {
        return None;
    }
    resp.bytes().await.ok().map(|b| b.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zipped(csv: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(Cursor::new(&mut buf));
            zw.start_file("BhavCopy_NSE_FO.csv", zip::write::SimpleFileOptions::default())
                .unwrap();
            zw.write_all(csv.as_bytes()).unwrap();
            zw.finish().unwrap();
        }
        buf
    }

    #[test]
    fn option_symbol_builds_the_master_form() {
        assert_eq!(
            option_symbol("NIFTY", "2026-10-30", "25000", "CE").as_deref(),
            Some("NIFTY30OCT2625000CE")
        );
        // Fractional strike kept.
        assert_eq!(
            option_symbol("VEDL", "2026-04-25", "292.5", "PE").as_deref(),
            Some("VEDL25APR26292.5PE")
        );
    }

    #[test]
    fn parse_reads_closing_oi_by_symbol() {
        let csv = "TradDt,TckrSymb,XpryDt,StrkPric,OptnTp,OpnIntrst\n\
                   2026-10-29,NIFTY,2026-10-30,25000,CE,123456\n\
                   2026-10-29,NIFTY,2026-10-30,25000,PE,654321\n\
                   2026-10-29,NIFTY,2026-10-30,25000,XX,999\n";
        let (oi, underlyings) = parse_bhavcopy(&zipped(csv)).expect("parse");
        assert_eq!(oi.get("NIFTY30OCT2625000CE"), Some(&123456.0));
        assert_eq!(oi.get("NIFTY30OCT2625000PE"), Some(&654321.0));
        assert!(underlyings.contains("NIFTY"));
        // The XX (non CE/PE) row is ignored.
        assert_eq!(oi.len(), 2);
    }

    #[test]
    fn underlying_of_strips_the_contract() {
        assert_eq!(underlying_of("NIFTY30OCT2625000CE").as_deref(), Some("NIFTY"));
        assert_eq!(underlying_of("VEDL25APR26292.5PE").as_deref(), Some("VEDL"));
    }
}
