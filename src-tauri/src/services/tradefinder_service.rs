// services/tradefinder_service.rs
//! TradeFinder market_pulse + sector_scope client -- a read-only proxy for the
//! three ranked lists (intraday_boost, breakout_beacon, high_powered_stocks)
//! and the sector-scope index, so the frontend never has to hold or paste a
//! TradeFinder token. Port of the fork's `services/tradefinder_service.py`.
//!
//! Auth: a `jwttoken` the operator pastes into `<data_dir>/tf_jwt.txt` (or the
//! `TF_JWT_TOKEN` env var) plus an `accesstoken` TOTP (RFC 6238, HMAC-SHA1,
//! 30s window). Read fresh on every call so a manual token refresh takes effect
//! on the next request with no restart.
//!
//! No I/O happens until a token is present: an empty token returns None before
//! any network call, so a caller (and the e2e tests) can exercise the
//! "no session configured" path entirely offline. The automated browser-based
//! JWT refresh the fork runs under Xvfb is NOT ported here -- Tauri has no Xvfb,
//! so the operator keeps the token fresh by hand until that is redesigned.

use crate::services::core::Reply;
use crate::state::AppState;
use data_encoding::BASE32;
use hmac::{Hmac, Mac};
use serde_json::{json, Map, Value};
use sha1::Sha1;
use std::path::PathBuf;
use tracing::warn;

type HmacSha1 = Hmac<Sha1>;

const TF_BASE_DEFAULT: &str = "https://tradefinder.in/api_be";
const TF_SECRET_DEFAULT: &str = "5ACHPKZUZNTWYPSJXNP7IULMACAM6P6Q";

fn tf_base() -> String {
    std::env::var("TF_BASE").unwrap_or_else(|_| TF_BASE_DEFAULT.to_string())
}

fn tf_secret() -> String {
    std::env::var("TF_SECRET").unwrap_or_else(|_| TF_SECRET_DEFAULT.to_string())
}

fn jwt_path(ctx: &AppState) -> PathBuf {
    match std::env::var("TF_JWT_FILE") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => ctx.data_dir.join("tf_jwt.txt"),
    }
}

/// Read the JWT fresh on every call (file first, then env fallback) so a manual
/// refresh takes effect on the very next tick -- no cache, no restart needed.
pub fn get_tf_jwt(ctx: &AppState) -> String {
    if let Ok(s) = std::fs::read_to_string(jwt_path(ctx)) {
        let t = s.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    std::env::var("TF_JWT_TOKEN").unwrap_or_default()
}

/// Whether a server-side JWT is present at all -- lets a caller tell "no token
/// to authenticate with" apart from "token was fine, the request itself
/// failed", so the no-session message never masquerades as a token expiry.
pub fn has_tf_jwt(ctx: &AppState) -> bool {
    !get_tf_jwt(ctx).is_empty()
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// RFC 6238 TOTP -- HMAC-SHA1, 30s window, 6 digits. Pulled out with an explicit
/// secret argument so it can be checked against the published test vectors.
fn totp_with_secret(secret: &str, ts_ms: i64, step: i64) -> String {
    // Faithful to the source's `int(ts_ms/1000/step)`: float division then
    // truncate. The sub-second fraction never crosses a whole-second step
    // boundary, so this equals integer division, but the form is kept 1:1.
    let counter = ((ts_ms as f64) / 1000.0 / step as f64).floor() as u64;
    let up = secret.to_uppercase();
    let pad = (8 - up.len() % 8) % 8;
    let padded = format!("{up}{}", "=".repeat(pad));
    let key = match BASE32.decode(padded.as_bytes()) {
        Ok(k) => k,
        Err(_) => return "000000".to_string(),
    };
    let mut mac = <HmacSha1 as Mac>::new_from_slice(&key).expect("hmac accepts any key length");
    mac.update(&counter.to_be_bytes());
    let h = mac.finalize().into_bytes();
    let offset = (h[h.len() - 1] & 0x0f) as usize;
    let code = ((h[offset] as u32 & 0x7f) << 24)
        | ((h[offset + 1] as u32) << 16)
        | ((h[offset + 2] as u32) << 8)
        | (h[offset + 3] as u32);
    format!("{:06}", code % 1_000_000)
}

async fn server_time_ms(ctx: &AppState) -> i64 {
    let url = format!("{}/servertime", tf_base());
    let resp = match ctx.http.get(&url).send().await {
        Ok(r) => r,
        Err(_) => return now_ms(),
    };
    let v: Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return now_ms(),
    };
    let data = v.get("payload").and_then(|p| p.get("data"));
    data.and_then(Value::as_i64)
        .or_else(|| data.and_then(Value::as_str).and_then(|s| s.parse().ok()))
        .unwrap_or_else(now_ms)
}

fn safe_float(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Symbol->symbol, param_0->ltp, param_1->prev_close, param_2->change_pct,
/// param_3->score. Each field is converted independently (non-numeric -> 0.0)
/// rather than all-or-nothing, so one off-schema field (e.g. a "BEAR" sentiment
/// label on breakout_beacon) never drops the whole row. Sorted by |score| desc.
fn map_items(raw: &[Value]) -> Vec<Value> {
    let mut items: Vec<Value> = raw
        .iter()
        .filter_map(|it| {
            let symbol = it.get("Symbol").and_then(Value::as_str)?;
            if symbol.is_empty() {
                return None;
            }
            Some(json!({
                "symbol": symbol,
                "ltp": safe_float(it.get("param_0")),
                "prev_close": safe_float(it.get("param_1")),
                "change_pct": safe_float(it.get("param_2")),
                "score": safe_float(it.get("param_3")),
            }))
        })
        .collect();
    items.sort_by(|a, b| {
        let sa = a.get("score").and_then(Value::as_f64).unwrap_or(0.0).abs();
        let sb = b.get("score").and_then(Value::as_f64).unwrap_or(0.0).abs();
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });
    items
}

// Response keys can vary; fall back through known alternates (matches the
// frontend TS client's mapRawItems()/fetchMarketPulse() behaviour).
const LIST_KEYS: [(&str, &[&str]); 3] = [
    ("intraday_boost", &["intraday_boost"]),
    ("breakout_beacon", &["breakout_beacon", "beacon", "breakout"]),
    ("high_powered_stocks", &["high_powered_stocks", "high_powered", "powered"]),
];

/// Fetch all three TradeFinder ranked lists in one call. Returns None on ANY
/// failure (empty/expired JWT, network error, TF error payload) so the caller
/// can report a clean "no session" rather than writing partial data.
pub async fn fetch_market_pulse(ctx: &AppState) -> Option<Map<String, Value>> {
    let jwt = get_tf_jwt(ctx);
    if jwt.is_empty() {
        return None;
    }
    let totp = totp_with_secret(&tf_secret(), server_time_ms(ctx).await, 30);
    let url = format!("{}/data/market_pulse", tf_base());
    let data: Value = match ctx
        .http
        .get(&url)
        .header("jwttoken", &jwt)
        .header("accesstoken", &totp)
        .send()
        .await
    {
        Ok(r) => match r.json().await {
            Ok(v) => v,
            Err(e) => {
                warn!("TradeFinder market_pulse response was not readable: {e}");
                return None;
            }
        },
        Err(e) => {
            warn!("TradeFinder market_pulse request failed: {e}");
            return None;
        }
    };
    if data.get("status").and_then(Value::as_str) == Some("ERROR") {
        warn!("TradeFinder market_pulse returned an error payload (token expired?)");
        return None;
    }
    let payload_data = data
        .get("payload")
        .and_then(|p| p.get("data"))
        .cloned()
        .unwrap_or(Value::Null);

    let mut result = Map::new();
    for (list_type, alt_keys) in LIST_KEYS {
        let mut found: Vec<Value> = Vec::new();
        for key in alt_keys {
            if let Some(arr) = payload_data.get(key).and_then(Value::as_array) {
                if !arr.is_empty() {
                    found = arr.clone();
                    break;
                }
            }
        }
        result.insert(list_type.to_string(), Value::Array(map_items(&found)));
    }
    Some(result)
}

/// Fetch the sector-scope index + per-sector stock breakdown. Same all-or-
/// nothing contract as market_pulse. Uses `Authorization: Bearer` (the route TF
/// moved sector-scope to in 2026-09) rather than the legacy `jwttoken` header.
pub async fn fetch_sector_scope(ctx: &AppState) -> Option<Map<String, Value>> {
    let jwt = get_tf_jwt(ctx);
    if jwt.is_empty() {
        return None;
    }
    let totp = totp_with_secret(&tf_secret(), server_time_ms(ctx).await, 30);
    let url = format!("{}/data/sector_scope", tf_base());
    let resp = match ctx
        .http
        .get(&url)
        .header("Authorization", format!("Bearer {jwt}"))
        .header("accesstoken", &totp)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!("TradeFinder sector_scope request failed: {e}");
            return None;
        }
    };
    if !resp.status().is_success() {
        warn!(
            "TradeFinder sector_scope returned HTTP {} -- a TF-side route change or token rejection",
            resp.status().as_u16()
        );
        return None;
    }
    let data: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            warn!("TradeFinder sector_scope response was not readable: {e}");
            return None;
        }
    };
    if data.get("status").and_then(Value::as_str) == Some("ERROR") {
        return None;
    }
    let payload_data = data
        .get("payload")
        .and_then(|p| p.get("data"))
        .cloned()
        .unwrap_or(Value::Null);
    let mut out = Map::new();
    out.insert(
        "index".to_string(),
        payload_data
            .get("daily-index")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
    );
    out.insert(
        "sectors".to_string(),
        payload_data
            .get("all_sector")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new())),
    );
    Some(out)
}

/// The message a trader reads when a pulse/scope fetch yields nothing. Names the
/// cause and the next action -- no status codes, no protocol terms. Distinguishes
/// "no token at all" (operator must add one) from "token present but the fetch
/// failed" (transient, nothing for them to fix).
fn unavailable_message(ctx: &AppState) -> &'static str {
    if has_tf_jwt(ctx) {
        "The TradeFinder session could not be reached right now. It will keep retrying; no action is needed."
    } else {
        "No TradeFinder session is configured on this server yet. Add your TradeFinder token to turn on the ranked lists."
    }
}

/// Build the market_pulse reply. Success carries the three ranked lists under
/// `data`; the no-session path is a 200 with `status: error` and a trader-facing
/// message, exactly as the fork does, so the client can fall back cleanly rather
/// than treat it as a hard failure. The per-item enrichments (CPR, first-candle,
/// directional, future-liquidity, momentum) are NOT attached here -- each needs
/// broker history and is a separate follow-up; their fields are simply absent.
pub async fn market_pulse_reply(ctx: &AppState) -> Reply {
    match fetch_market_pulse(ctx).await {
        Some(data) => Reply::ok(json!({"status": "success", "data": Value::Object(data)})),
        None => Reply::ok(json!({"status": "error", "message": unavailable_message(ctx)})),
    }
}

/// Build the sector_scope reply. Same shape and no-session contract as
/// [`market_pulse_reply`].
pub async fn sector_scope_reply(ctx: &AppState) -> Reply {
    match fetch_sector_scope(ctx).await {
        Some(data) => Reply::ok(json!({"status": "success", "data": Value::Object(data)})),
        None => Reply::ok(json!({"status": "error", "message": unavailable_message(ctx)})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totp_matches_the_rfc6238_sha1_test_vector() {
        // RFC 6238 Appendix B: ASCII secret "12345678901234567890" (base32
        // "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"), SHA1, at T=59s the 8-digit TOTP
        // is 94287082, so the 6-digit code is its low 6 digits: 287082.
        let secret = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
        assert_eq!(totp_with_secret(secret, 59_000, 30), "287082");
        // T=1111111109s -> 8-digit 07081804 -> 6-digit 081804.
        assert_eq!(totp_with_secret(secret, 1_111_111_109_000, 30), "081804");
    }

    #[test]
    fn totp_pads_a_non_multiple_of_eight_secret() {
        // The production secret is 32 chars (a clean multiple of 8), but the
        // padding path must not panic on a shorter one.
        let out = totp_with_secret("MFRGG", 0, 30); // "MFRGG" -> needs padding
        assert_eq!(out.len(), 6);
        assert!(out.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn map_items_maps_fields_and_sorts_by_absolute_score() {
        let raw = vec![
            json!({"Symbol": "A", "param_0": 100.0, "param_1": 99.0, "param_2": 1.0, "param_3": 2.0}),
            json!({"Symbol": "B", "param_0": "200.5", "param_1": "198", "param_2": "1.2", "param_3": "-5.0"}),
            json!({"Symbol": "", "param_3": 9.0}),
            json!({"param_3": 9.0}),
        ];
        let out = map_items(&raw);
        // Empty/missing symbol rows are dropped; B sorts first on |score| 5 > 2.
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["symbol"], json!("B"));
        assert_eq!(out[0]["ltp"], json!(200.5)); // numeric string coerced
        assert_eq!(out[0]["score"], json!(-5.0));
        assert_eq!(out[1]["symbol"], json!("A"));
    }

    #[test]
    fn safe_float_coerces_strings_and_defaults_garbage_to_zero() {
        assert_eq!(safe_float(Some(&json!("3.5"))), 3.5);
        assert_eq!(safe_float(Some(&json!(7))), 7.0);
        assert_eq!(safe_float(Some(&json!("BEAR"))), 0.0);
        assert_eq!(safe_float(Some(&Value::Null)), 0.0);
        assert_eq!(safe_float(None), 0.0);
    }
}
