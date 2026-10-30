//! TradeFinder proxy endpoints: `/api/v1/tfmarketpulse` and
//! `/api/v1/tfsectorscope`. A valid apikey only (no broker session) -- the data
//! is a TradeFinder subscription the server authenticates with its own token,
//! not an account call. Port of the fork's `restx_api/tf_market_pulse.py` and
//! `restx_api/tf_sector_scope.py`, base lists only (the per-item CPR / momentum
//! enrichments are a later follow-up and are simply absent until then).

use super::{check, send, Auth, Style};
use crate::server::envelope::read_json_object;
use crate::server::middleware::ClientIp;
use crate::services::{schemas, tradefinder_service};
use crate::state::AppState;
use axum::{
    extract::{Request, State},
    response::Response,
};
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

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
