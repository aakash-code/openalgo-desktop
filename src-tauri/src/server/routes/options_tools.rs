//! The options tool pages (web blueprints `oiprofile.py`, `oitracker.py`,
//! `ivchart.py`, `gamma_density.py`, `straddle_chart.py`,
//! `custom_straddle.py`, `vol_surface.py`, `gex.py`, `ivsmile.py`,
//! `arbitrage.py`, `strategy_chart.py`, `strike_profile.py`). Handlers
//! check the session's prerequisites, validate like the web and call one
//! service.

use crate::server::api_v1::send;
use crate::server::envelope::{error, EarlyReply};
use crate::server::routes::webui::{ok, JsonBody};
use crate::services::apikey_service::ApiKeyService;
use crate::services::chain_tools_service as chain;
use crate::services::history_tools_service::{self as hist, ChartRequest, SimParams};
use crate::services::options_service::Expiry;
use crate::services::strike_profile_service::{self as strike_profile, ProfileRequest};
use crate::services::{arbitrage_service, market_data_service};
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Response,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

pub const NO_KEY: &str = "API key not configured. Please generate an API key in /apikey";
pub const NO_KEY_SHORT: &str = "API key not configured";
pub const NO_BROKER: &str = "Connect your broker to load this data.";
const BAD_DAYS: &str = "Enter the number of days as a whole number.";

/// The web refuses a tool when the trader has no API key (401).
fn need_key(ctx: &AppState, msg: &str) -> Option<Response> {
    match ApiKeyService::current(ctx) {
        Ok(Some(_)) => None,
        Ok(None) => Some(error(StatusCode::UNAUTHORIZED, msg)),
        Err(e) => {
            tracing::error!("Reading the API key failed: {}", e);
            Some(error(StatusCode::UNAUTHORIZED, msg))
        }
    }
}

/// Market data needs a connected broker.
fn need_broker(ctx: &AppState) -> Option<Response> {
    (!ctx.is_broker_connected()).then(|| error(StatusCode::BAD_REQUEST, NO_BROKER))
}

/// A string field, trimmed (`data.get(k, "").strip()`); anything that is
/// not a string reads as empty.
fn text(b: &JsonBody, k: &str, max: usize) -> String {
    match b.0.get(k) {
        Some(Value::String(s)) => s.trim().chars().take(max).collect(),
        _ => String::new(),
    }
}

/// `int(data.get(k, default))`.
fn int(b: &JsonBody, k: &str, default: i64) -> Result<i64, EarlyReply> {
    match b.int(k) {
        Ok(v) => Ok(v.unwrap_or(default)),
        Err(_) => Err(error(StatusCode::BAD_REQUEST, BAD_DAYS).into()),
    }
}

fn upper_alnum(s: &str, underscore: bool) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || (underscore && c == b'_'))
}

/// Web `^\d{2}[A-Z]{3}\d{2}$`, and a real calendar day (the web lets
/// `31FEB26` through to an empty lookup).
fn is_ddmmmyy(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 7
        && b[..2].iter().all(u8::is_ascii_digit)
        && b[2..5].iter().all(u8::is_ascii_uppercase)
        && b[5..].iter().all(u8::is_ascii_digit)
        && Expiry::parse(s).is_some()
}

pub const BAD_EXPIRY: &str = "Invalid expiry_date format. Expected DDMMMYY";

/// A history tool's expiry, checked before any master or broker lookup
/// (the web passes it on unchecked; a multibyte one split a character in
/// the strike lookup). Upper-cased like the chain tools'.
fn history_expiry(raw: &str) -> Result<String, EarlyReply> {
    Expiry::parse(raw)
        .map(|e| e.compact().to_string())
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, BAD_EXPIRY).into())
}

/// The chain tools' shared request: underlying (20), exchange (20) and a
/// `DDMMMYY` expiry (10), all required and upper case.
fn chain_request(b: &JsonBody) -> Result<(String, String, String), EarlyReply> {
    let (u, e, x) = (
        text(b, "underlying", 20),
        text(b, "exchange", 20),
        text(b, "expiry_date", 10),
    );
    if u.is_empty() || e.is_empty() || x.is_empty() {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "underlying, exchange, and expiry_date are required",
        )
        .into());
    }
    if !upper_alnum(&u, false) || !upper_alnum(&e, true) {
        return Err(error(StatusCode::BAD_REQUEST, "Invalid input format").into());
    }
    if !is_ddmmmyy(&x) {
        return Err(error(StatusCode::BAD_REQUEST, BAD_EXPIRY).into());
    }
    Ok((u, e, x))
}

/// Key, request and broker checks of a chain tool, in the web's order.
fn chain_prologue(ctx: &AppState, b: &JsonBody) -> Result<(String, String, String), EarlyReply> {
    if let Some(r) = need_key(ctx, NO_KEY) {
        return Err(r.into());
    }
    let req = chain_request(b)?;
    if let Some(r) = need_broker(ctx) {
        return Err(r.into());
    }
    Ok(req)
}

macro_rules! chain_route {
    ($name:ident, $svc:path) => {
        pub async fn $name(State(ctx): Ctx, body: JsonBody) -> Response {
            match chain_prologue(&ctx, &body) {
                Ok((u, e, x)) => send($svc(&ctx, &u, &e, &x).await),
                Err(r) => *r,
            }
        }
    };
}

chain_route!(oi_data, chain::oi_data);
chain_route!(max_pain, chain::max_pain);
chain_route!(gex_data, chain::gex);
chain_route!(iv_smile_data, chain::iv_smile);
chain_route!(gamma_data, chain::gamma_density);

/// POST /oiprofile/api/profile-data
pub async fn profile_data(State(ctx): Ctx, body: JsonBody) -> Response {
    if let Some(r) = need_key(&ctx, NO_KEY) {
        return r;
    }
    let mut interval = text(&body, "interval", 5);
    if !body.0.contains_key("interval") {
        interval = "5m".into();
    }
    let days = match int(&body, "days", 5) {
        Ok(d) => d.min(30),
        Err(r) => return *r,
    };
    let (u, e, x) = match chain_request(&body) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    if !["1m", "5m", "15m"].contains(&interval.as_str()) {
        return error(
            StatusCode::BAD_REQUEST,
            "Invalid interval. Allowed: 15m, 1m, 5m",
        );
    }
    if let Some(r) = need_broker(&ctx) {
        return r;
    }
    // Optional multi-expiry sum (Sensibull-style): a list of ticked expiries
    // summed per strike. Absent or one entry falls through to the single expiry.
    let expiry_dates: Vec<String> = body
        .0
        .get("expiry_dates")
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if expiry_dates.len() > 1 {
        send(chain::oi_profile_multi(&ctx, &u, &e, &expiry_dates, &interval, days).await)
    } else {
        send(chain::oi_profile(&ctx, &u, &e, &x, &interval, days).await)
    }
}

/// Python's `str()` of a JSON field, as far as it decides which refusal
/// the web gives: a number keeps its digits, `null` reads `None`, `true`
/// reads `True`; a missing field is empty (`data.get(k, "")`).
fn py_str(v: Option<&Value>) -> String {
    match v {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) => "None".into(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => "False".into(),
        Some(other) => other.to_string(),
    }
}

fn bad_request(msg: &str) -> EarlyReply {
    error(StatusCode::BAD_REQUEST, msg).into()
}

/// The strike window: 5% when not given, every loaded strike for `null`,
/// otherwise one of 2, 5 and 10 (`float()` of a number or a string).
fn range_pct(v: Option<&Value>) -> Result<Option<f64>, EarlyReply> {
    let pct = match v {
        None => 5.0,
        Some(Value::Null) => return Ok(None),
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => s.trim().parse::<f64>().unwrap_or(f64::NAN),
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(_) => f64::NAN,
    };
    if strike_profile::ALLOWED_RANGES.contains(&pct) {
        Ok(Some(pct))
    } else {
        Err(bad_request("Invalid strike range"))
    }
}

/// The Strike Profile request (web `strike_profile.py`), refused in the
/// web's order: underlying and exchange (20 characters each), the expiry
/// list (the first [`strike_profile::MAX_EXPIRIES`], upper-cased), the
/// strike window.
fn strike_profile_request(b: &JsonBody) -> Result<ProfileRequest, EarlyReply> {
    let field = |k: &str| -> String { py_str(b.0.get(k)).trim().chars().take(20).collect() };
    let (underlying, exchange) = (field("underlying"), field("exchange"));
    if underlying.is_empty() || exchange.is_empty() {
        return Err(bad_request("underlying and exchange are required"));
    }
    if !upper_alnum(&underlying, false) || !upper_alnum(&exchange, true) {
        return Err(bad_request("Invalid input format"));
    }
    let expiries: Vec<String> = match b.0.get("expiry_dates") {
        Some(Value::Array(a)) if !a.is_empty() => a
            .iter()
            .take(strike_profile::MAX_EXPIRIES)
            .map(|e| py_str(Some(e)).trim().to_uppercase())
            .collect(),
        _ => return Err(bad_request("Select at least one expiry.")),
    };
    if !expiries.iter().all(|e| is_ddmmmyy(e)) {
        return Err(bad_request("Invalid expiry format. Expected DDMMMYY"));
    }
    Ok(ProfileRequest {
        underlying,
        exchange,
        expiries,
        range_pct: range_pct(b.0.get("range_pct"))?,
    })
}

/// POST /strikeprofile/api/profile-data
pub async fn strike_profile_data(State(ctx): Ctx, body: JsonBody) -> Response {
    if let Some(r) = need_key(&ctx, NO_KEY) {
        return r;
    }
    let req = match strike_profile_request(&body) {
        Ok(r) => r,
        Err(r) => return *r,
    };
    if let Some(r) = need_broker(&ctx) {
        return r;
    }
    send(strike_profile::strike_profile(&ctx, &req).await)
}

/// The broker's intervals, or the refusal.
fn intervals_of(ctx: &AppState, key_msg: &str) -> Result<Value, EarlyReply> {
    if let Some(r) = need_key(ctx, key_msg) {
        return Err(r.into());
    }
    if let Some(r) = need_broker(ctx) {
        return Err(r.into());
    }
    let r = market_data_service::intervals(ctx);
    if r.is_success() {
        Ok(r.body)
    } else {
        Err(send(r).into())
    }
}

fn bucket(v: &Value, k: &str) -> Vec<Value> {
    v.get("data")
        .and_then(|d| d.get(k))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// GET /oiprofile/api/intervals: the broker's 1m, 5m and 15m only.
pub async fn oiprofile_intervals(State(ctx): Ctx) -> Response {
    match intervals_of(&ctx, NO_KEY) {
        Ok(v) => {
            let keep: Vec<Value> = bucket(&v, "minutes")
                .into_iter()
                .filter(|i| matches!(i.as_str(), Some("1m" | "5m" | "15m")))
                .collect();
            ok(json!({"status": "success", "data": {"intervals": keep}}))
        }
        Err(r) => *r,
    }
}

/// GET /ivchart/api/intervals: intraday buckets only.
pub async fn ivchart_intervals(State(ctx): Ctx) -> Response {
    match intervals_of(&ctx, NO_KEY) {
        Ok(v) => ok(json!({"status": "success", "data": {
            "seconds": bucket(&v, "seconds"),
            "minutes": bucket(&v, "minutes"),
            "hours": bucket(&v, "hours"),
        }})),
        Err(r) => *r,
    }
}

/// GET /straddle/api/intervals, /straddlepnl/api/intervals,
/// /strategybuilder/api/intervals: the broker's full interval list.
pub async fn all_intervals(State(ctx): Ctx) -> Response {
    match intervals_of(&ctx, NO_KEY_SHORT) {
        Ok(v) => ok(v),
        Err(r) => *r,
    }
}

/// Underlying, exchange, expiry of the history tools; the expiry must be a
/// real `DDMMMYY` day.
fn history_prologue(ctx: &AppState, b: &JsonBody) -> Result<(String, String, String), EarlyReply> {
    if let Some(r) = need_broker(ctx) {
        return Err(r.into());
    }
    if let Some(r) = need_key(ctx, NO_KEY) {
        return Err(r.into());
    }
    let (u, e, x) = (
        text(b, "underlying", 64),
        text(b, "exchange", 20),
        text(b, "expiry_date", 10),
    );
    if u.is_empty() || e.is_empty() || x.is_empty() {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "underlying, exchange, and expiry_date are required",
        )
        .into());
    }
    Ok((u, e, history_expiry(&x)?))
}

fn interval_or(b: &JsonBody, default: &str) -> String {
    match b.0.get("interval") {
        Some(Value::String(s)) => s.trim().chars().take(10).collect(),
        _ => default.to_string(),
    }
}

/// Days for the history tools, kept to 1..=30 so one request cannot ask
/// the broker for years of candles per leg.
fn clamp_days(d: i64) -> i64 {
    d.clamp(1, 30)
}

/// POST /ivchart/api/iv-data
pub async fn iv_data(State(ctx): Ctx, body: JsonBody) -> Response {
    let (u, e, x) = match history_prologue(&ctx, &body) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let days = match int(&body, "days", 1) {
        Ok(d) => clamp_days(d),
        Err(r) => return *r,
    };
    let interval = interval_or(&body, "5m");
    send(hist::iv_chart(&ctx, &u, &e, &x, &interval, days).await)
}

/// POST /ivchart/api/default-symbols
pub async fn default_symbols(State(ctx): Ctx, body: JsonBody) -> Response {
    if let Some(r) = need_key(&ctx, NO_KEY) {
        return r;
    }
    let (u, e, x) = (
        text(&body, "underlying", 64),
        text(&body, "exchange", 20),
        text(&body, "expiry_date", 10),
    );
    if u.is_empty() || e.is_empty() || x.is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "underlying, exchange, and expiry_date are required",
        );
    }
    let x = match history_expiry(&x) {
        Ok(x) => x,
        Err(r) => return *r,
    };
    if let Some(r) = need_broker(&ctx) {
        return r;
    }
    send(hist::default_symbols(&ctx, &u, &e, &x).await)
}

/// POST /straddle/api/straddle-data
pub async fn straddle_data(State(ctx): Ctx, body: JsonBody) -> Response {
    let (u, e, x) = match history_prologue(&ctx, &body) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let days = match int(&body, "days", 5) {
        Ok(d) => clamp_days(d),
        Err(r) => return *r,
    };
    let interval = interval_or(&body, "1m");
    send(hist::straddle_chart(&ctx, &u, &e, &x, &interval, days).await)
}

/// POST /straddlepnl/api/simulate
pub async fn simulate(State(ctx): Ctx, body: JsonBody) -> Response {
    let (u, e, x) = match history_prologue(&ctx, &body) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let num = |k: &str, d: i64| int(&body, k, d);
    let (days, adj, lot, lots) = match (
        num("days", 1),
        num("adjustment_points", 50),
        num("lot_size", 65),
        num("lots", 1),
    ) {
        (Ok(a), Ok(b), Ok(c), Ok(d)) => (a, b, c, d),
        _ => {
            return error(
                StatusCode::BAD_REQUEST,
                "Enter whole numbers for days, adjustment points, lot size and lots.",
            )
        }
    };
    if adj < 1 {
        return error(StatusCode::BAD_REQUEST, "adjustment_points must be >= 1");
    }
    if lot < 1 || lots < 1 {
        return error(StatusCode::BAD_REQUEST, "lot_size and lots must be >= 1");
    }
    let interval = interval_or(&body, "1m");
    let p = SimParams {
        days: clamp_days(days),
        adjustment_points: adj,
        lot_size: lot,
        lots,
    };
    send(hist::custom_straddle(&ctx, &u, &e, &x, &interval, p).await)
}

/// GET /straddlepnl/api/lotsize?underlying&exchange
pub async fn lotsize(State(ctx): Ctx, Query(q): Query<HashMap<String, String>>) -> Response {
    let get = |k: &str| {
        q.get(k)
            .map(|s| s.trim().to_ascii_uppercase())
            .unwrap_or_default()
    };
    let (u, e) = (get("underlying"), get("exchange"));
    if u.is_empty() || e.is_empty() {
        return error(StatusCode::BAD_REQUEST, "underlying and exchange required");
    }
    ok(json!({"status": "success", "lotsize": hist::lot_size(&ctx, &u, &e)}))
}

/// POST /volsurface/api/surface-data
pub async fn surface_data(State(ctx): Ctx, body: JsonBody) -> Response {
    if let Some(r) = need_broker(&ctx) {
        return r;
    }
    if let Some(r) = need_key(&ctx, NO_KEY) {
        return r;
    }
    let (u, e) = (text(&body, "underlying", 64), text(&body, "exchange", 20));
    let strike_count = match int(&body, "strike_count", 15) {
        Ok(n) => n.clamp(5, 40) as usize,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "Enter the strike count as a whole number.",
            )
        }
    };
    if u.is_empty() || e.is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "underlying and exchange are required",
        );
    }
    let expiries: Vec<String> = match body.0.get("expiry_dates") {
        Some(Value::Array(a)) if !a.is_empty() => a
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .take(8)
            .collect(),
        _ => {
            return error(
                StatusCode::BAD_REQUEST,
                "expiry_dates must be a non-empty list",
            )
        }
    };
    let expiries = match expiries
        .iter()
        .map(|x| history_expiry(x))
        .collect::<Result<Vec<String>, EarlyReply>>()
    {
        Ok(v) => v,
        Err(r) => return *r,
    };
    send(hist::vol_surface(&ctx, &u, &e, &expiries, strike_count).await)
}

/// GET /arbitrage/api/universe?exchanges=NFO,MCX
pub async fn arbitrage_universe(
    State(ctx): Ctx,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if let Some(r) = need_key(&ctx, NO_KEY) {
        return r;
    }
    let raw = q.get("exchanges").map(|s| s.trim()).unwrap_or("");
    let exchanges: Vec<String> = if raw.is_empty() {
        arbitrage_service::DEFAULT_EXCHANGES
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        raw.split(',')
            .map(|e| e.trim().to_ascii_uppercase())
            .filter(|e| !e.is_empty())
            .collect()
    };
    let snap = ctx.symbols.snapshot();
    send(arbitrage_service::universe(
        snap.rows(),
        &exchanges,
        ctx.now(),
    ))
}

/// The Strategy Builder chart body (web `strategy_chart.py`).
fn chart_request(ctx: &AppState, b: &JsonBody) -> Result<ChartRequest, EarlyReply> {
    if let Some(r) = need_broker(ctx) {
        return Err(r.into());
    }
    if let Some(r) = need_key(ctx, NO_KEY) {
        return Err(r.into());
    }
    let opt = |k: &str| Some(text(b, k, 64)).filter(|s| !s.is_empty());
    let underlying = text(b, "underlying", 64);
    let exchange = text(b, "exchange", 20);
    let interval = opt("interval").unwrap_or_else(|| "5m".into());
    let days = b.int("days").ok().flatten().unwrap_or(3).clamp(1, 30);
    let legs = match b.0.get("legs") {
        Some(Value::Array(a)) => a.clone(),
        None | Some(Value::Null) => Vec::new(),
        Some(_) => {
            return Err(error(StatusCode::BAD_REQUEST, "At least one leg is required").into())
        }
    };
    if underlying.is_empty() || exchange.is_empty() {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "underlying and exchange are required",
        )
        .into());
    }
    if legs.is_empty() {
        return Err(error(StatusCode::BAD_REQUEST, "At least one leg is required").into());
    }
    Ok(ChartRequest {
        underlying,
        exchange,
        underlying_symbol: opt("underlying_symbol"),
        underlying_exchange: opt("underlying_exchange"),
        interval,
        days,
        start_date: opt("start_date"),
        end_date: opt("end_date"),
        legs,
    })
}

/// POST /strategybuilder/api/strategy-chart
pub async fn strategy_chart(State(ctx): Ctx, body: JsonBody) -> Response {
    match chart_request(&ctx, &body) {
        Ok(req) => send(hist::strategy_chart(&ctx, &req).await),
        Err(r) => *r,
    }
}

/// POST /strategybuilder/api/multi-strike-oi
pub async fn multi_strike_oi(State(ctx): Ctx, body: JsonBody) -> Response {
    match chart_request(&ctx, &body) {
        Ok(req) => send(hist::multi_strike_oi(&ctx, &req).await),
        Err(r) => *r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert!(is_ddmmmyy("30OCT26"));
        assert!(!is_ddmmmyy("30oct26"));
        assert!(!is_ddmmmyy("30-OCT-26"));
        assert!(upper_alnum("NIFTY50", false));
        assert!(!upper_alnum("NSE_INDEX", false));
        assert!(upper_alnum("NSE_INDEX", true));
        assert!(!upper_alnum("nifty", false));
        assert_eq!(clamp_days(0), 1);
        assert_eq!(clamp_days(999), 30);
    }

    /// Web `str()` then `float()` of the Strike Profile fields.
    #[test]
    fn strike_profile_fields() {
        assert_eq!(py_str(None), "");
        assert_eq!(py_str(Some(&json!(5))), "5");
        assert_eq!(py_str(Some(&json!(null))), "None");
        assert_eq!(py_str(Some(&json!(true))), "True");
        let range = |v: Option<Value>| range_pct(v.as_ref()).ok();
        assert_eq!(range(None), Some(Some(5.0)));
        assert_eq!(range(Some(json!(null))), Some(None));
        assert_eq!(range(Some(json!(10))), Some(Some(10.0)));
        assert_eq!(range(Some(json!(" 2 "))), Some(Some(2.0)));
        assert_eq!(range(Some(json!(2.5))), None);
        assert_eq!(range(Some(json!("abc"))), None);
        assert_eq!(range(Some(json!(true))), None);
        assert_eq!(range(Some(json!([5]))), None);
    }
}
