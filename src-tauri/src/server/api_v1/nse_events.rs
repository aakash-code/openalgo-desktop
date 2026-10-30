//! NSE corporate-events endpoint.

use super::{check, send, Auth, Style};
use crate::server::envelope::read_json_object;
use crate::server::middleware::ClientIp;
use crate::services::{nse_events_service, schemas};
use crate::state::AppState;
use axum::{
    extract::{Request, State},
    response::Response,
};
use serde_json::Value;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// POST /api/v1/nseevents
///
/// A valid key only (no broker session): the data is public NSE calendar, not
/// an account call. `symbols` is read from the raw body because it is a list of
/// bare strings, which the typed loader has no kind for; the schema validates
/// the key and excludes the rest.
pub async fn nseevents(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    let body = match read_json_object(req, &()).await {
        Ok(b) => b,
        Err(r) => return *r,
    };
    if let Err(r) = check(
        &ctx,
        ip,
        &body,
        schemas::nse_events(),
        Style::Object,
        Auth::KeyOnly,
    )
    .await
    {
        return *r;
    }
    let symbols: Vec<String> = body
        .get("symbols")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let within_days = body.get("within_days").and_then(Value::as_i64).unwrap_or(2);
    send(nse_events_service::events(&ctx, &symbols, within_days).await)
}
