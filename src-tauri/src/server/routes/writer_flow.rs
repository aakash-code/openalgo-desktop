//! Writer Flow web routes (session-authed), port of the fork's
//! `blueprints/writer_flow.py`:
//!
//! - `GET /writerflow/api/sessions` -- underlyings and expiries recorded, newest day first
//! - `GET /writerflow/api/flow`     -- the day read strike by strike, for the page and the chart
//!
//! The data comes from [`crate::services::writer_flow_service`]; these read the
//! recorded store and start nothing (on-demand recording arrives with the
//! recorder). A read failure answers with `status: "error"` and a trader-facing
//! line, never a code.

use super::{Access, RouteSpec};
use crate::server::routes::webui::ok;
use crate::services::market_read_core::{build_read, ReadInputs};
use crate::services::oi_flow_db::{available_sessions, load_day};
use crate::services::oi_levels::{build_levels, LevelInputs};
use crate::services::writer_flow_service::{flow_answer, recorder_enabled, FlowView};
use axum::{extract::Query, http::Method, response::Response, routing::get};
use chrono::NaiveDate;
use serde_json::{json, Value};
use std::collections::HashMap;

type Q = Query<HashMap<String, String>>;

pub fn table() -> Vec<RouteSpec> {
    vec![
        RouteSpec {
            path: "/writerflow/api/sessions",
            method: Method::GET,
            access: Access::User,
            make: || get(api_sessions),
        },
        RouteSpec {
            path: "/writerflow/api/flow",
            method: Method::GET,
            access: Access::User,
            make: || get(api_flow),
        },
        RouteSpec {
            path: "/writerflow/api/levels",
            method: Method::GET,
            access: Access::User,
            make: || get(api_levels),
        },
        RouteSpec {
            path: "/writerflow/api/read",
            method: Method::GET,
            access: Access::User,
            make: || get(api_read),
        },
    ]
}

/// GET /writerflow/api/read -- the once-a-minute Market Read for the recorded
/// day: who leads option writing, support/resistance, and price (futures,
/// India VIX and IV are null until a live quote feed is wired).
pub async fn api_read(Query(q): Q) -> Response {
    let underlying = arg(&q, "underlying").unwrap_or("").to_uppercase();
    if underlying.is_empty() {
        return ok(json!({"status": "error", "message": "Choose an underlying, such as NIFTY."}));
    }
    let expiry = arg(&q, "expiry").unwrap_or("").to_uppercase();
    let day = arg(&q, "date").and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
    let sessions = match available_sessions(10) {
        Ok(s) => s,
        Err(_) => return ok(json!({"status": "error", "message": READ_FAILED})),
    };
    let chosen = sessions
        .into_iter()
        .filter(|s| s.underlying == underlying)
        .filter(|s| day.map(|d| s.trade_date == d.to_string()).unwrap_or(true))
        .find(|s| expiry.is_empty() || s.expiry == expiry);
    let Some(chosen) = chosen else {
        return ok(json!({
            "status": "empty",
            "message": "Nothing recorded for this yet.",
            "recorder_enabled": recorder_enabled(),
        }));
    };
    let Ok(trade_date) = NaiveDate::parse_from_str(&chosen.trade_date, "%Y-%m-%d") else {
        return ok(json!({"status": "error", "message": READ_FAILED}));
    };
    let samples = match load_day(&underlying, &chosen.expiry, trade_date) {
        Ok(s) => s,
        Err(_) => return ok(json!({"status": "error", "message": READ_FAILED})),
    };
    let inputs = ReadInputs {
        underlying: underlying.clone(),
        samples,
        prev_close: None,
    };
    match build_read(&inputs) {
        Some(mut v) => {
            if let Some(o) = v.as_object_mut() {
                o.insert("status".into(), json!("success"));
            }
            ok(v)
        }
        None => ok(json!({
            "status": "empty",
            "message": "Not enough recorded yet for a read.",
            "recorder_enabled": recorder_enabled(),
        })),
    }
}

/// GET /writerflow/api/levels -- support/resistance zones, walls, touches and
/// hold rates for the recorded day (Option Levels; gamma deferred).
pub async fn api_levels(Query(q): Q) -> Response {
    let underlying = arg(&q, "underlying").unwrap_or("").to_uppercase();
    if underlying.is_empty() {
        return ok(json!({"status": "error", "message": "Choose an underlying, such as NIFTY."}));
    }
    let expiry = arg(&q, "expiry").unwrap_or("").to_uppercase();
    let day = arg(&q, "date").and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
    let sessions = match available_sessions(10) {
        Ok(s) => s,
        Err(_) => return ok(json!({"status": "error", "message": READ_FAILED})),
    };
    let chosen = sessions
        .into_iter()
        .filter(|s| s.underlying == underlying)
        .filter(|s| day.map(|d| s.trade_date == d.to_string()).unwrap_or(true))
        .find(|s| expiry.is_empty() || s.expiry == expiry);
    let Some(chosen) = chosen else {
        return ok(json!({
            "status": "empty",
            "message": "Nothing recorded for this yet.",
            "recorder_enabled": recorder_enabled(),
        }));
    };
    let Ok(trade_date) = NaiveDate::parse_from_str(&chosen.trade_date, "%Y-%m-%d") else {
        return ok(json!({"status": "error", "message": READ_FAILED}));
    };
    let samples = match load_day(&underlying, &chosen.expiry, trade_date) {
        Ok(s) => s,
        Err(_) => return ok(json!({"status": "error", "message": READ_FAILED})),
    };
    let inputs = LevelInputs {
        underlying: underlying.clone(),
        samples,
        expiry_day: false,
        past_touches: Vec::new(),
    };
    match build_levels(&inputs) {
        Some(mut v) => {
            if let Some(o) = v.as_object_mut() {
                o.insert("status".into(), json!("success"));
            }
            ok(v)
        }
        None => ok(json!({
            "status": "empty",
            "message": "Not enough recorded yet to read levels.",
            "recorder_enabled": recorder_enabled(),
        })),
    }
}

fn arg<'a>(q: &'a HashMap<String, String>, k: &str) -> Option<&'a str> {
    q.get(k).map(|s| s.trim()).filter(|s| !s.is_empty())
}

/// "YYYY-MM-DDTHH:MM:SS" -> "HH:MM".
fn hhmm_of(ts: &str) -> String {
    ts.get(11..16).unwrap_or(ts).to_string()
}

const READ_FAILED: &str = "Could not read the Writer Flow record. Try again in a moment.";

/// GET /writerflow/api/sessions
pub async fn api_sessions() -> Response {
    match available_sessions(10) {
        Ok(rows) => {
            let sessions: Vec<Value> = rows
                .iter()
                .map(|s| {
                    json!({
                        "trade_date": s.trade_date,
                        "underlying": s.underlying,
                        "expiry": s.expiry,
                        "minutes": s.minutes,
                        "last_sample": s.last_sample.as_deref().map(hhmm_of),
                    })
                })
                .collect();
            ok(json!({
                "status": "success",
                "recorder_enabled": recorder_enabled(),
                "sessions": sessions,
            }))
        }
        Err(_) => ok(json!({"status": "error", "message": READ_FAILED})),
    }
}

/// GET /writerflow/api/flow
pub async fn api_flow(Query(q): Q) -> Response {
    let underlying = arg(&q, "underlying").unwrap_or("").to_uppercase();
    let expiry = arg(&q, "expiry").unwrap_or("").to_uppercase();
    let step = arg(&q, "step").unwrap_or("auto").to_lowercase();
    if underlying.is_empty() {
        return ok(json!({"status": "error", "message": "Choose an underlying, such as NIFTY."}));
    }
    if !matches!(step.as_str(), "auto" | "60" | "180" | "300") {
        return ok(json!({"status": "error", "message": "The step can be Auto, 1, 3 or 5 minutes."}));
    }
    let day = arg(&q, "date").and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
    let noise_pct: f64 = arg(&q, "noise_pct").and_then(|s| s.parse().ok()).unwrap_or(0.5);
    let noise_lots: i64 = arg(&q, "noise_lots").and_then(|s| s.parse().ok()).unwrap_or(5);
    let quiet: f64 = arg(&q, "quiet_minutes").and_then(|s| s.parse().ok()).unwrap_or(5.0);
    if !(0.0..=50.0).contains(&noise_pct)
        || !(0..=10000).contains(&noise_lots)
        || !(1.0..=60.0).contains(&quiet)
    {
        return ok(json!({"status": "error", "message": "One of the settings is out of range."}));
    }
    let truthy = |k: &str| {
        matches!(
            arg(&q, k).map(|s| s.to_lowercase()).as_deref(),
            Some("1" | "true" | "yes" | "on")
        )
    };
    let view = FlowView {
        underlying,
        expiry,
        day,
        step,
        noise_pct,
        noise_lots,
        quiet_minutes: quiet,
        all_strikes: truthy("all_strikes"),
        lite: truthy("lite"),
    };
    match flow_answer(&view) {
        Ok(payload) => ok(payload),
        Err(_) => ok(json!({"status": "error", "message": READ_FAILED})),
    }
}
