//! Writer Flow's per-minute OI recorder: once a minute during market hours it
//! pulls the option chain around ATM for each configured underlying and stores
//! the open interest and premium, so [`crate::services::writer_flow_service`]
//! can read the day back without touching the broker again. Port of the fork's
//! `services/oi_flow_recorder_service.py` (the live tick; on-demand recording,
//! backfill and gap-fill are follow-ups).
//!
//! Opt-in via `OI_FLOW_RECORDER_ENABLED=true`. Reads nothing and records
//! nothing when a broker is not connected (the chain call simply fails and the
//! minute is skipped), so it is safe to leave on.
//!
//! No socket event yet: the Writer Flow page and chart poll every 60 s, which
//! picks up each stored minute; a `writer_flow_update` push is a later refinement.

use crate::services::oi_flow_db::{self, FlowRow};
use crate::services::options_service::{option_chain, option_exchange};
use crate::services::search_ui_service;
use crate::events::subscribers::UiEmitter;
use crate::services::writer_flow_service::{rebuild_watched, recorder_enabled};
use crate::state::AppState;
use chrono::{Datelike, NaiveTime, Timelike};
use chrono_tz::Asia::Kolkata;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// `OI_FLOW_UNDERLYINGS`, default the three index chains.
fn configured_underlyings() -> Vec<(String, String)> {
    let raw = std::env::var("OI_FLOW_UNDERLYINGS")
        .unwrap_or_else(|_| "NIFTY:NSE_INDEX,BANKNIFTY:NSE_INDEX,SENSEX:BSE_INDEX".to_string());
    raw.split(',')
        .filter_map(|e| {
            let mut it = e.splitn(2, ':');
            let base = it.next()?.trim().to_uppercase();
            let exch = it.next().unwrap_or("NSE_INDEX").trim().to_uppercase();
            if base.is_empty() {
                None
            } else {
                Some((base, exch))
            }
        })
        .collect()
}

fn strikes_around_atm() -> i64 {
    std::env::var("OI_FLOW_STRIKES_AROUND_ATM")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(15)
        .clamp(1, 50)
}

/// Weekday, 09:15-15:40 IST. Index options trade to 15:40; the MCX close is
/// handled by a later gap-fill, not this live tick.
fn in_market_hours(now_ist: chrono::NaiveDateTime) -> bool {
    let wd = now_ist.weekday().number_from_monday();
    if wd > 5 {
        return false;
    }
    let t = now_ist.time();
    t >= NaiveTime::from_hms_opt(9, 15, 0).unwrap() && t <= NaiveTime::from_hms_opt(15, 40, 0).unwrap()
}

/// Start the recorder (an owned task; the process exit stops it). A no-op
/// unless `OI_FLOW_RECORDER_ENABLED=true`.
pub fn start(ctx: &Arc<AppState>) {
    if !recorder_enabled() {
        return;
    }
    if let Err(e) = oi_flow_db::init(&ctx.data_dir) {
        tracing::warn!("Writer Flow recorder: could not open its store: {e}");
        return;
    }
    let ctx = ctx.clone();
    tokio::spawn(async move {
        tracing::info!("Writer Flow recorder started");
        loop {
            sleep_to_next_minute().await;
            let now_ist = ctx.now().with_timezone(&Kolkata).naive_local();
            if !in_market_hours(now_ist) {
                continue;
            }
            if let Err(e) = tick(&ctx, now_ist).await {
                tracing::warn!("Writer Flow recorder tick failed: {e}");
            }
        }
    });
}

/// Sleep until five seconds past the next minute, so a tick reads a chain the
/// feed has settled for the minute just closed.
async fn sleep_to_next_minute() {
    let now = chrono::Utc::now();
    let secs = now.second() as u64;
    let wait = 60u64.saturating_sub(secs) + 5;
    tokio::time::sleep(Duration::from_secs(wait)).await;
}

/// One minute's record for every configured underlying.
async fn tick(ctx: &Arc<AppState>, now_ist: chrono::NaiveDateTime) -> crate::error::Result<()> {
    let strike_count = strikes_around_atm();
    let mut recorded: Vec<String> = Vec::new();
    for (base, index_exchange) in configured_underlyings() {
        match record_one(ctx, &base, &index_exchange, strike_count, now_ist).await {
            Ok(true) => recorded.push(base.clone()),
            Ok(false) => {}
            Err(e) => tracing::debug!("Writer Flow recorder: {base} skipped: {e}"),
        }
    }
    if !recorded.is_empty() {
        rebuild_watched(&recorded);
        // Tell open Writer Flow charts/pages a new minute landed, so they
        // refetch at once rather than waiting for the next 60s poll.
        for base in &recorded {
            ctx.ui
                .emit("writer_flow_update", serde_json::json!({ "underlying": base }))
                .await;
        }
    }
    Ok(())
}

/// Record one underlying's chain this minute. Returns whether rows were stored.
async fn record_one(
    ctx: &Arc<AppState>,
    base: &str,
    index_exchange: &str,
    strike_count: i64,
    now_ist: chrono::NaiveDateTime,
) -> crate::error::Result<bool> {
    let opt_ex = option_exchange(index_exchange);
    let snap = ctx.symbols.snapshot();
    let expiries = search_ui_service::expiries(
        snap.rows(),
        Some(opt_ex.as_str()),
        Some(base),
        Some("options"),
        ctx.now(),
    );
    let Some(expiry) = expiries.into_iter().next() else {
        return Ok(false);
    };
    drop(snap);

    // The chain call takes the underlying's own (index) exchange and derives
    // the option exchange itself; it needs a broker session for quotes.
    let reply = option_chain(ctx, base, index_exchange, &expiry, Some(strike_count), false, None).await;
    if reply.status != 200 {
        return Ok(false);
    }
    let body = reply.body;
    if body.get("status").and_then(Value::as_str) != Some("success") {
        return Ok(false);
    }
    let spot = body.get("underlying_ltp").and_then(Value::as_f64);
    let Some(chain) = body.get("chain").and_then(Value::as_array) else {
        return Ok(false);
    };

    let trade_date = now_ist.date();
    let mut rows: Vec<FlowRow> = Vec::new();
    for row in chain {
        let Some(strike) = row.get("strike").and_then(Value::as_f64) else { continue };
        for side in ["ce", "pe"] {
            let Some(leg) = row.get(side).and_then(Value::as_object) else { continue };
            let Some(symbol) = leg.get("symbol").and_then(Value::as_str) else { continue };
            rows.push(FlowRow {
                trade_date,
                sample_time: now_ist,
                underlying: base.to_string(),
                exchange: opt_ex.clone(),
                expiry: expiry.clone(),
                strike,
                side: side.to_uppercase(),
                symbol: symbol.to_string(),
                oi: leg.get("oi").and_then(Value::as_i64),
                ltp: leg.get("ltp").and_then(Value::as_f64),
                volume: leg.get("volume").and_then(Value::as_i64),
                spot,
                lot_size: leg.get("lotsize").and_then(Value::as_i64).map(|v| v as i32),
                source: "live".to_string(),
            });
        }
    }
    if rows.is_empty() {
        return Ok(false);
    }
    let n = oi_flow_db::insert_samples(&rows)?;
    Ok(n > 0)
}
