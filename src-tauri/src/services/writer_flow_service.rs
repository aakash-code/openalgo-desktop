//! Writer Flow: the day read strike by strike, as the page and the /trading
//! chart draw it. Builds the answer from the recorded samples
//! ([`crate::services::oi_flow_db`]) through the pure analysis core
//! ([`crate::services::oi_flow_analysis`]), and keeps it until the underlying's
//! write count ([`oi_flow_db::generation`]) moves, so a refresh with nothing
//! new recorded costs nothing. Port of the fork's `services/writer_flow_service.py`.

use crate::error::Result;
use crate::services::oi_flow_analysis::{analyse, bias, FlowSettings};
use crate::services::oi_flow_db::{self, available_sessions, generation, load_day};
use chrono::{NaiveDate, NaiveDateTime};
use chrono_tz::Asia::Kolkata;
use chrono::TimeZone;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// What one request asks for, already validated.
#[derive(Clone, Debug)]
pub struct FlowView {
    pub underlying: String,
    pub expiry: String,
    pub day: Option<NaiveDate>,
    /// "auto" | "60" | "180" | "300".
    pub step: String,
    pub noise_pct: f64,
    pub noise_lots: i64,
    pub quiet_minutes: f64,
    pub all_strikes: bool,
    /// The chart draws no per-cell reading, so it leaves out the largest field.
    pub lite: bool,
}

impl Default for FlowView {
    fn default() -> Self {
        Self {
            underlying: String::new(),
            expiry: String::new(),
            day: None,
            step: "auto".to_string(),
            noise_pct: 0.5,
            noise_lots: 5,
            quiet_minutes: 5.0,
            all_strikes: false,
            lite: false,
        }
    }
}

impl FlowView {
    fn cache_key(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.underlying,
            self.expiry,
            self.day.map(|d| d.to_string()).unwrap_or_default(),
            self.step,
            self.noise_pct,
            self.noise_lots,
            self.quiet_minutes,
            self.all_strikes,
            self.lite
        )
    }
}

fn step_seconds(step: &str) -> Option<i64> {
    match step {
        "60" => Some(60),
        "180" => Some(180),
        "300" => Some(300),
        _ => None, // "auto"
    }
}

fn class_code(cls: &str) -> &'static str {
    match cls {
        "writing" => "W",
        "covering" => "C",
        "buying" => "B",
        "unwinding" => "U",
        "adding" => "A",
        "reducing" => "R",
        _ => "S",
    }
}

fn hhmm(t: NaiveDateTime) -> String {
    t.format("%H:%M").to_string()
}

/// UTC seconds for a recorded time, which is stored as IST wall clock. The
/// chart places everything by UTC seconds, the same clock as its bars.
fn epoch(t: NaiveDateTime) -> i64 {
    Kolkata
        .from_local_datetime(&t)
        .single()
        .map(|dt| dt.timestamp())
        .unwrap_or(0)
}

/// `OI_FLOW_RECORDER_ENABLED=true`.
pub fn recorder_enabled() -> bool {
    std::env::var("OI_FLOW_RECORDER_ENABLED")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[derive(Clone)]
struct Cached {
    generation: u64,
    payload: Value,
}

static CACHE: Mutex<Option<HashMap<String, Cached>>> = parking_lot::const_mutex(None);
/// Views a request asked for, so a write can rebuild only what is on screen.
static ASKED: Mutex<Option<HashMap<String, FlowView>>> = parking_lot::const_mutex(None);

/// The answer for a request: from memory while nothing new is recorded, else
/// built once. Records the view as watched so a later write rebuilds it.
pub fn flow_answer(view: &FlowView) -> Result<Value> {
    {
        let mut asked = ASKED.lock();
        asked
            .get_or_insert_with(HashMap::new)
            .insert(view.cache_key(), view.clone());
    }
    answer(view)
}

fn answer(view: &FlowView) -> Result<Value> {
    let key = view.cache_key();
    let gen = generation(&view.underlying);
    if let Some(c) = CACHE.lock().as_ref().and_then(|m| m.get(&key)) {
        if c.generation == gen {
            return Ok(c.payload.clone());
        }
    }
    let payload = flow_payload(view)?;
    CACHE.lock().get_or_insert_with(HashMap::new).insert(
        key,
        Cached {
            generation: gen,
            payload: payload.clone(),
        },
    );
    Ok(payload)
}

/// After a write: rebuild the watched views of these underlyings. Never fails
/// the caller; a view that errors is left to be rebuilt on its next request.
pub fn rebuild_watched(underlyings: &[String]) -> usize {
    let views: Vec<FlowView> = ASKED
        .lock()
        .as_ref()
        .map(|m| m.values().cloned().collect())
        .unwrap_or_default();
    let mut rebuilt = 0;
    for view in views {
        if underlyings.contains(&view.underlying) && answer(&view).is_ok() {
            rebuilt += 1;
        }
    }
    rebuilt
}

/// The day read strike by strike. `status: "empty"` when nothing is recorded.
pub fn flow_payload(view: &FlowView) -> Result<Value> {
    let mut held: Vec<oi_flow_db::Session> = available_sessions(10)?
        .into_iter()
        .filter(|s| s.underlying == view.underlying)
        .filter(|s| view.day.map(|d| s.trade_date == d.to_string()).unwrap_or(true))
        .filter(|s| view.expiry.is_empty() || s.expiry == view.expiry)
        .collect();
    if held.is_empty() {
        let reason = if recorder_enabled() {
            "Nothing recorded for this yet. Open its chart with Writer Flow on to start recording it, or list it in OI_FLOW_UNDERLYINGS."
        } else {
            "The Writer Flow recorder is off. Set OI_FLOW_RECORDER_ENABLED=true in .env and restart."
        };
        return Ok(json!({
            "status": "empty",
            "message": reason,
            "recorder_enabled": recorder_enabled(),
        }));
    }
    let chosen = held.remove(0);
    let trade_date = NaiveDate::parse_from_str(&chosen.trade_date, "%Y-%m-%d")
        .map_err(|_| crate::error::AppError::Internal("bad recorded trade_date".into()))?;

    let settings = FlowSettings {
        step_seconds: step_seconds(&view.step),
        noise_pct: view.noise_pct,
        noise_lots: view.noise_lots,
        quiet_minutes_to_break: view.quiet_minutes,
        ..Default::default()
    };
    let samples = load_day(&view.underlying, &chosen.expiry, trade_date)?;
    let out = analyse(&samples, &settings);

    let step_index: HashMap<NaiveDateTime, usize> =
        out.steps.iter().enumerate().map(|(i, t)| (*t, i)).collect();

    // By default only strikes sampled at the latest step; the chart asks for all.
    let rows: Vec<&_> = out
        .strikes
        .iter()
        .filter(|r| view.all_strikes || r.observed)
        .collect();
    let strikes: Vec<f64> = rows.iter().map(|r| r.strike).collect();
    let strike_index: HashMap<u64, usize> = strikes
        .iter()
        .enumerate()
        .map(|(i, k)| (k.to_bits(), i))
        .collect();

    let n_steps = out.steps.len();
    let mut heat_ce = vec![vec![Value::Null; n_steps]; strikes.len()];
    let mut heat_pe = vec![vec![Value::Null; n_steps]; strikes.len()];
    let mut code_ce = vec![vec![String::new(); n_steps]; strikes.len()];
    let mut code_pe = vec![vec![String::new(); n_steps]; strikes.len()];
    for c in &out.cells {
        let Some(&i) = strike_index.get(&c.strike.to_bits()) else { continue };
        let Some(&j) = step_index.get(&c.time) else { continue };
        let signed = bias(&c.side, &c.cls) as i64 * c.oi_change.abs();
        if c.side == "CE" {
            heat_ce[i][j] = json!(signed);
            code_ce[i][j] = class_code(&c.cls).to_string();
        } else {
            heat_pe[i][j] = json!(signed);
            code_pe[i][j] = class_code(&c.cls).to_string();
        }
    }

    let times: Vec<String> = out.steps.iter().map(|t| hhmm(*t)).collect();
    let times_epoch: Vec<i64> = out.steps.iter().map(|t| epoch(*t)).collect();

    let totals: Vec<Value> = out
        .totals
        .iter()
        .map(|t| {
            json!({
                "time": hhmm(t.time),
                "epoch": epoch(t.time),
                "spot": t.spot,
                "ce_change": t.ce_change,
                "pe_change": t.pe_change,
            })
        })
        .collect();

    let ladder: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "strike": r.strike,
                "atm": r.atm.unwrap_or(false),
                "observed": r.observed,
                "ce": side_state("CE", r.ce.as_ref()),
                "pe": side_state("PE", r.pe.as_ref()),
            })
        })
        .collect();

    let events: Vec<Value> = out
        .events
        .iter()
        .map(|e| {
            json!({
                "time": hhmm(e.time),
                "epoch": epoch(e.time),
                "strike": e.strike,
                "side": e.side,
                "kind": e.kind,
                "minutes": e.minutes,
                "oi_change": e.oi_change,
                "bias": bias(&e.side, &e.kind),
            })
        })
        .collect();

    let mut payload = Map::new();
    payload.insert("status".into(), json!("success"));
    payload.insert("underlying".into(), json!(view.underlying));
    payload.insert("expiry".into(), json!(chosen.expiry));
    payload.insert("date".into(), json!(chosen.trade_date));
    payload.insert("recorder_enabled".into(), json!(recorder_enabled()));
    payload.insert("step_seconds".into(), json!(out.step_seconds));
    payload.insert("oi_unit".into(), json!(out.oi_unit));
    payload.insert("spot".into(), json!(out.spot));
    payload.insert("times".into(), json!(times));
    payload.insert("times_epoch".into(), json!(times_epoch));
    payload.insert("strikes".into(), json!(strikes));
    payload.insert("heat".into(), json!({"ce": heat_ce, "pe": heat_pe}));
    payload.insert("totals".into(), json!(totals));
    payload.insert("ladder".into(), json!(ladder));
    payload.insert("events".into(), json!(events));
    if !view.lite {
        payload.insert("heat_cls".into(), json!({"ce": code_ce, "pe": code_pe}));
    }
    Ok(Value::Object(payload))
}

fn side_state(
    side: &str,
    state: Option<&crate::services::oi_flow_analysis::SideState>,
) -> Value {
    let Some(s) = state else { return Value::Null };
    let window: Map<String, Value> = s
        .window_change
        .iter()
        .map(|(k, v)| (k.to_string(), json!(v)))
        .collect();
    json!({
        "cls": s.cls,
        "streak_minutes": s.streak_minutes,
        "streak_oi_change": s.streak_oi_change,
        "steady_minutes": s.steady_minutes,
        "oi": s.oi,
        "observed": s.observed,
        "bias": bias(side, &s.cls),
        "window_change": window,
        "last_seen": s.last_seen.map(hhmm),
    })
}
