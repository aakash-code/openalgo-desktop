//! TradeFinder proxy endpoints: `/api/v1/tfmarketpulse` and
//! `/api/v1/tfsectorscope`. A valid apikey only (no broker session) -- the data
//! is a TradeFinder subscription the server authenticates with its own token,
//! not an account call. Port of the fork's `restx_api/tf_market_pulse.py` and
//! `restx_api/tf_sector_scope.py`, base lists only (the per-item CPR / momentum
//! enrichments are a later follow-up and are simply absent until then).

use super::{check, send, Auth, Style};
use crate::server::envelope::read_json_object;
use crate::server::middleware::ClientIp;
use crate::services::{schemas, tf_boost_movement_service, tradefinder_service};
use crate::state::AppState;
use axum::{
    extract::{Request, State},
    response::Response,
};
use serde_json::Value;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// `list_type` from the body, defaulting to the Intraday Boost list.
fn list_type_of(body: &serde_json::Map<String, Value>) -> String {
    body.get("list_type")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("intraday_boost")
        .to_string()
}

fn str_field<'a>(body: &'a serde_json::Map<String, Value>, key: &str) -> &'a str {
    body.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// POST /api/v1/tfmarketpulse
pub async fn tfmarketpulse(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    let body = match read_json_object(req, &()).await {
        Ok(b) => b,
        Err(r) => return *r,
    };
    if let Err(r) = check(
        &ctx,
        ip,
        &body,
        schemas::apikey_only(),
        Style::Object,
        Auth::KeyOnly,
    )
    .await
    {
        return *r;
    }
    send(tradefinder_service::market_pulse_reply(&ctx).await)
}

/// POST /api/v1/tfsectorscope
pub async fn tfsectorscope(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    let body = match read_json_object(req, &()).await {
        Ok(b) => b,
        Err(r) => return *r,
    };
    if let Err(r) = check(
        &ctx,
        ip,
        &body,
        schemas::apikey_only(),
        Style::Object,
        Auth::KeyOnly,
    )
    .await
    {
        return *r;
    }
    send(tradefinder_service::sector_scope_reply(&ctx).await)
}

/// POST /api/v1/boostmovement
///
/// Current-day rank-movement rows (delta, velocity, acceleration, Top-N,
/// sustained zone, event) reconstructed from the snapshot history by the pure
/// movement engine -- no upstream fetch. Degrades to an empty list, never an
/// error, when there are no snapshots for the day.
pub async fn boostmovement(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    let body = match read_json_object(req, &()).await {
        Ok(b) => b,
        Err(r) => return *r,
    };
    if let Err(r) = check(
        &ctx,
        ip,
        &body,
        schemas::tf_boost_query(),
        Style::Object,
        Auth::KeyOnly,
    )
    .await
    {
        return *r;
    }
    let date = str_field(&body, "date");
    let list_type = list_type_of(&body);
    send(tf_boost_movement_service::movement_reply(&ctx, date, &list_type))
}

/// POST /api/v1/boostsnapshots
///
/// The day's recorded symbols for a list, with optional rank/price timelines.
pub async fn boostsnapshots(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    let body = match read_json_object(req, &()).await {
        Ok(b) => b,
        Err(r) => return *r,
    };
    if let Err(r) = check(
        &ctx,
        ip,
        &body,
        schemas::tf_boost_query(),
        Style::Object,
        Auth::KeyOnly,
    )
    .await
    {
        return *r;
    }
    let date = str_field(&body, "date");
    let list_type = list_type_of(&body);
    let include_ranks = body.get("includeRanks").and_then(Value::as_bool).unwrap_or(false);
    let include_prices = body.get("includePrices").and_then(Value::as_bool).unwrap_or(false);
    send(tf_boost_movement_service::snapshots_reply(
        &ctx,
        date,
        &list_type,
        include_ranks,
        include_prices,
    ))
}
