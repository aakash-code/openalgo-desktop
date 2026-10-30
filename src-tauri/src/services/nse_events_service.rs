//! NSE corporate-event client -- board meetings, results and corporate
//! actions, keyed by symbol so a scanner row can be flagged before it is
//! traded.
//!
//! Server-side because nseindia.com rejects API calls without the cookies its
//! homepage sets, and a browser cannot warm that session cross-origin; one
//! fetch here also serves every client instead of each tab hammering NSE.
//!
//! Cached for the IST trading day: results and board meetings are announced
//! days ahead and do not change intraday, so a single fetch is enough, and the
//! cache key is the IST date so the first request after midnight refetches.
//!
//! Degrades quietly. NSE rate-limits and intermittently 401s one endpoint
//! while serving the other, so a partial calendar is kept rather than lost, and
//! a failed fetch returns an empty map rather than an error so a caller's own
//! flow never breaks. Port of the fork's `services/nse_events_service.py`.

use crate::services::core::Reply;
use crate::state::AppState;
use chrono::{NaiveDate, NaiveDateTime, Utc};
use chrono_tz::Asia::Kolkata;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::time::Duration;

const NSE_BASE: &str = "https://www.nseindia.com";
const TIMEOUT: Duration = Duration::from_secs(15);

// Board meetings carry the "purpose" text (Results / Dividend / Fund Raising),
// corporate actions carry ex-dates for splits, bonuses and dividends.
const ENDPOINTS: &[(&str, &str)] = &[
    ("board_meeting", "/api/event-calendar"),
    (
        "corporate_action",
        "/api/corporates-corporateActions?index=equities",
    ),
];

#[derive(Clone)]
struct Event {
    kind: String,
    date: NaiveDate,
    purpose: String,
}

#[derive(Default)]
struct Cache {
    date: Option<NaiveDate>,
    events: HashMap<String, Vec<Event>>,
}

static CACHE: Mutex<Option<Cache>> = parking_lot::const_mutex(None);

fn today_ist() -> NaiveDate {
    Utc::now().with_timezone(&Kolkata).date_naive()
}

/// NSE mixes formats across endpoints. Returns the date, or None when the
/// field is unparseable: an event with no usable date is dropped rather than
/// guessed, since a wrong date on a trading flag is worse than no flag.
fn parse_date(value: &str) -> Option<NaiveDate> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let head: String = trimmed.chars().take(20).collect();
    for fmt in ["%d-%b-%Y", "%d-%m-%Y", "%Y-%m-%d"] {
        if let Ok(d) = NaiveDate::parse_from_str(&head, fmt) {
            return Some(d);
        }
    }
    NaiveDateTime::parse_from_str(&head, "%d-%b-%Y %H:%M:%S")
        .ok()
        .map(|dt| dt.date())
}

/// Browser headers NSE expects; without a close-enough User-Agent and Referer
/// the `/api/` calls 401 even with cookies.
fn browser_headers() -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE, REFERER, USER_AGENT};
    let mut h = HeaderMap::new();
    h.insert(
        USER_AGENT,
        HeaderValue::from_static(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/120.0 Safari/537.36",
        ),
    );
    h.insert(
        ACCEPT,
        HeaderValue::from_static("application/json, text/plain, */*"),
    );
    h.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.9"));
    h.insert(
        REFERER,
        HeaderValue::from_static(
            "https://www.nseindia.com/companies-listing/corporate-filings-event-calendar",
        ),
    );
    h
}

/// `name=value; name=value` from a warm-up response's Set-Cookie headers.
/// reqwest is built without the cookie-store feature, so the session is warmed
/// by hand: NSE's homepage sets the cookies its `/api/` calls then require.
fn join_cookies(headers: &reqwest::header::HeaderMap) -> String {
    headers
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|c| c.split(';').next())
        .filter(|c| !c.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

async fn collect(
    http: &reqwest::Client,
    cookie: &str,
    kind: &str,
    path: &str,
) -> reqwest::Result<Vec<(String, Event)>> {
    let mut headers = browser_headers();
    if !cookie.is_empty() {
        if let Ok(v) = reqwest::header::HeaderValue::from_str(cookie) {
            headers.insert(reqwest::header::COOKIE, v);
        }
    }
    let body: Value = http
        .get(format!("{NSE_BASE}{path}"))
        .headers(headers)
        .timeout(TIMEOUT)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    // Some endpoints return the list bare, others wrap it under "data".
    let rows = match &body {
        Value::Array(a) => a.clone(),
        Value::Object(o) => o.get("data").and_then(Value::as_array).cloned().unwrap_or_default(),
        _ => Vec::new(),
    };

    let mut out = Vec::new();
    for row in rows {
        let Some(obj) = row.as_object() else { continue };
        let symbol = obj
            .get("symbol")
            .or_else(|| obj.get("Symbol"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_uppercase();
        if symbol.is_empty() {
            continue;
        }
        let raw_date = obj
            .get("date")
            .or_else(|| obj.get("meetingDate"))
            .or_else(|| obj.get("exDate"))
            .or_else(|| obj.get("exdate"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(date) = parse_date(raw_date) else { continue };
        let purpose: String = obj
            .get("purpose")
            .or_else(|| obj.get("subject"))
            .or_else(|| obj.get("comp"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .chars()
            .take(120)
            .collect();
        out.push((
            symbol,
            Event {
                kind: kind.to_string(),
                date,
                purpose,
            },
        ));
    }
    Ok(out)
}

/// Whatever could be collected: one endpoint failing must not lose the other.
/// None only when the session could not be warmed at all.
async fn fetch_live(http: &reqwest::Client) -> Option<HashMap<String, Vec<Event>>> {
    let warm = http
        .get(NSE_BASE)
        .headers(browser_headers())
        .timeout(TIMEOUT)
        .send()
        .await
        .ok()?;
    let cookie = join_cookies(warm.headers());

    let mut events: HashMap<String, Vec<Event>> = HashMap::new();
    for (kind, path) in ENDPOINTS {
        if let Ok(rows) = collect(http, &cookie, kind, path).await {
            for (sym, ev) in rows {
                events.entry(sym).or_default().push(ev);
            }
        }
    }
    Some(events)
}

async fn events_map(http: &reqwest::Client) -> HashMap<String, Vec<Event>> {
    let today = today_ist();
    {
        let guard = CACHE.lock();
        if let Some(c) = guard.as_ref() {
            if c.date == Some(today) && !c.events.is_empty() {
                return c.events.clone();
            }
        }
    }
    let fetched = fetch_live(http).await.unwrap_or_default();
    if !fetched.is_empty() {
        *CACHE.lock() = Some(Cache {
            date: Some(today),
            events: fetched.clone(),
        });
    }
    fetched
}

/// symbol -> array of {kind, date, purpose} for the whole fetched calendar.
fn whole_calendar(all: &HashMap<String, Vec<Event>>) -> Value {
    let mut out = Map::new();
    for (sym, evs) in all {
        let arr: Vec<Value> = evs
            .iter()
            .map(|e| {
                json!({
                    "kind": e.kind,
                    "date": e.date.format("%Y-%m-%d").to_string(),
                    "purpose": e.purpose,
                })
            })
            .collect();
        out.insert(sym.clone(), Value::Array(arr));
    }
    Value::Object(out)
}

/// The single soonest event per requested symbol inside the window. The caller
/// draws a badge, not a calendar, so one event per symbol is enough.
fn soonest_per_symbol(all: &HashMap<String, Vec<Event>>, symbols: &[String], within: i64) -> Value {
    let today = today_ist();
    let horizon = today + chrono::Duration::days(within);
    let mut out = Map::new();
    for s in symbols {
        let key = s.trim().to_uppercase();
        if key.is_empty() {
            continue;
        }
        let mut upcoming: Vec<&Event> = all
            .get(&key)
            .map(|v| {
                v.iter()
                    .filter(|e| e.date >= today && e.date <= horizon)
                    .collect()
            })
            .unwrap_or_default();
        upcoming.sort_by_key(|e| e.date);
        if let Some(e) = upcoming.first() {
            out.insert(
                key,
                json!({
                    "kind": e.kind,
                    "date": e.date.format("%Y-%m-%d").to_string(),
                    "purpose": e.purpose,
                    "daysAway": (e.date - today).num_days(),
                }),
            );
        }
    }
    Value::Object(out)
}

/// `/api/v1/nseevents`. With symbols, the soonest event per symbol inside
/// `within_days` (0 = today only); without, the whole day's calendar. Always
/// `{status:success, data:...}` so an empty or failed fetch never breaks the
/// caller's flow.
pub async fn events(ctx: &AppState, symbols: &[String], within_days: i64) -> Reply {
    let within = within_days.clamp(0, 30);
    let all = events_map(&ctx.http).await;
    let data = if symbols.is_empty() {
        whole_calendar(&all)
    } else {
        soonest_per_symbol(&all, symbols, within)
    };
    Reply::ok(json!({"status": "success", "data": data}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nse_date_formats() {
        assert_eq!(
            parse_date("22-Sep-2026"),
            NaiveDate::from_ymd_opt(2026, 9, 22)
        );
        assert_eq!(
            parse_date("2026-09-22"),
            NaiveDate::from_ymd_opt(2026, 9, 22)
        );
        assert_eq!(
            parse_date("22-09-2026"),
            NaiveDate::from_ymd_opt(2026, 9, 22)
        );
        assert_eq!(
            parse_date("22-Sep-2026 10:30:00"),
            NaiveDate::from_ymd_opt(2026, 9, 22)
        );
        assert_eq!(parse_date(""), None);
        assert_eq!(parse_date("not a date"), None);
    }

    fn ev(kind: &str, date: NaiveDate, purpose: &str) -> Event {
        Event {
            kind: kind.to_string(),
            date,
            purpose: purpose.to_string(),
        }
    }

    #[test]
    fn soonest_picks_earliest_in_window_with_days_away() {
        let today = today_ist();
        let mut all: HashMap<String, Vec<Event>> = HashMap::new();
        all.insert(
            "INFY".into(),
            vec![
                ev("board_meeting", today + chrono::Duration::days(3), "Results"),
                ev("board_meeting", today + chrono::Duration::days(1), "Dividend"),
            ],
        );
        // Outside a 2-day window.
        all.insert(
            "SBIN".into(),
            vec![ev("board_meeting", today + chrono::Duration::days(9), "Results")],
        );

        let data = soonest_per_symbol(&all, &["infy".into(), "SBIN".into()], 2);
        let infy = &data["INFY"];
        assert_eq!(infy["daysAway"], json!(1));
        assert_eq!(infy["purpose"], json!("Dividend"));
        // SBIN's only event is past the horizon, so it is absent, not empty.
        assert!(data.get("SBIN").is_none());
    }
}
