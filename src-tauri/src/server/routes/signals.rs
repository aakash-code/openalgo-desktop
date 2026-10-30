//! Signal distribution API, mounted on its own `/signals/v1` surface (not
//! `/api/v1`), apikey in the body/query as that surface's convention. Stage 2:
//! ingest from the headless producer + cursor backfill. Subscriber webhooks
//! (HMAC-signed) and Telegram broadcast are later stages. Port of the fork's
//! `restx_api/signals.py`.

use super::{Access, RouteSpec};
use crate::server::envelope::json_response;
use crate::server::routes::webui::{ok, JsonBody};
use crate::services::apikey_service::ApiKeyService;
use crate::services::{schemas, signal_db, signal_ingest_service};
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::{Method, StatusCode},
    response::Response,
    routing::{get, post},
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

pub fn table() -> Vec<RouteSpec> {
    vec![
        RouteSpec {
            path: "/signals/v1/ingest",
            method: Method::POST,
            access: Access::Public,
            make: || post(ingest),
        },
        RouteSpec {
            path: "/signals/v1/events",
            method: Method::GET,
            access: Access::Public,
            make: || get(events),
        },
    ]
}

/// The body/query apikey matches the account's single key. Single-user
/// Desktop, so a plaintext compare is the whole check (no per-subscriber keys
/// until the subscriber stage).
fn key_matches(ctx: &AppState, key: &str) -> bool {
    !key.is_empty() && matches!(ApiKeyService::current(ctx), Ok(Some(k)) if k.expose() == key)
}

fn forbid() -> Response {
    json_response(
        StatusCode::FORBIDDEN,
        json!({"status": "error", "message": "Invalid openalgo apikey"}),
    )
}

/// POST /signals/v1/ingest
pub async fn ingest(State(ctx): Ctx, JsonBody(body): JsonBody) -> Response {
    let loaded = match schemas::signal_ingest().load(&body) {
        Ok(m) => m,
        Err(_) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                json!({"status": "error", "message": "Invalid signal payload."}),
            )
        }
    };
    let key = loaded.get("apikey").and_then(Value::as_str).unwrap_or_default();
    if !key_matches(&ctx, key) {
        return forbid();
    }
    let (_ok, body, status) = signal_ingest_service::ingest_signal(&ctx, &loaded);
    json_response(StatusCode::from_u16(status).unwrap_or(StatusCode::OK), body)
}

/// GET /signals/v1/events?apikey=..&since_id=0
pub async fn events(State(ctx): Ctx, Query(q): Query<HashMap<String, String>>) -> Response {
    let key = q.get("apikey").map(String::as_str).unwrap_or_default();
    if !key_matches(&ctx, key) {
        return forbid();
    }
    let since_id = q.get("since_id").and_then(|s| s.parse().ok()).unwrap_or(0);
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(200);
    match signal_db::get_events_since(&ctx, since_id, limit) {
        Ok(events) => {
            let last = events
                .last()
                .and_then(|e| e.get("id").and_then(Value::as_i64))
                .unwrap_or(since_id);
            ok(json!({"status": "success", "events": events, "last_id": last}))
        }
        Err(_) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"status": "error", "message": "Could not read signals."}),
        ),
    }
}
