//! TradeFinder web routes (session-authed), port of the fork's
//! `blueprints/tradefinder_proxy.py` settings endpoint:
//!
//! - `GET/POST /trading/tradefinder/settings` -- the TradeFinder background-work
//!   toggle, stored in the isolated boost store so it survives a restart.
//!
//! The market_pulse / sector_scope / boost proxies live on the apikey-authed
//! `/api/v1` surface (`server/api_v1/tradefinder.rs`); this is only the toggle
//! the terminal's TradeFinder panel reads and writes with the browser session.

use super::{Access, RouteSpec};
use crate::server::routes::webui::{ok, JsonBody};
use crate::services::tf_boost_db;
use crate::state::AppState;
use axum::{
    extract::State,
    http::Method,
    response::Response,
    routing::{get, post},
};
use serde_json::{json, Value};
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

const BACKGROUND_ENABLED: &str = "tf_background_enabled";

fn settings_json(ctx: &AppState) -> Value {
    let on = tf_boost_db::read_setting(ctx, BACKGROUND_ENABLED).ok().flatten().as_deref()
        == Some("true");
    json!({ "status": "success", "background_enabled": on })
}

/// GET /trading/tradefinder/settings
pub async fn settings_get(State(ctx): Ctx) -> Response {
    ok(settings_json(&ctx))
}

/// POST /trading/tradefinder/settings -- body `{background_enabled: bool}`.
pub async fn settings_post(State(ctx): Ctx, JsonBody(body): JsonBody) -> Response {
    if let Some(v) = body.get("background_enabled").and_then(Value::as_bool) {
        let _ = tf_boost_db::write_setting(&ctx, BACKGROUND_ENABLED, if v { "true" } else { "false" });
    }
    ok(settings_json(&ctx))
}

pub fn table() -> Vec<RouteSpec> {
    vec![
        RouteSpec {
            path: "/trading/tradefinder/settings",
            method: Method::GET,
            access: Access::User,
            make: || get(settings_get),
        },
        RouteSpec {
            path: "/trading/tradefinder/settings",
            method: Method::POST,
            access: Access::User,
            make: || post(settings_post),
        },
    ]
}
