//! Signal ingest: validate (at the route) then persist. Storage before any
//! broadcast keeps a producer retry idempotent. Telegram broadcast and
//! subscriber webhooks are a later stage. Port of the fork's
//! `services/signal_ingest_service.py` (persist half).

use crate::services::signal_db;
use crate::state::AppState;
use serde_json::{json, Map, Value};

/// Persist one validated signal. Returns (ok, body, status). A duplicate
/// `eventId` is a success (the retry path), not an error.
pub fn ingest_signal(ctx: &AppState, payload: &Map<String, Value>) -> (bool, Value, u16) {
    let event_id = payload.get("eventId").cloned().unwrap_or(Value::Null);
    match signal_db::insert_signal_event(ctx, payload) {
        Ok(Some(id)) => (
            true,
            json!({"status": "success", "id": id, "eventId": event_id}),
            200,
        ),
        Ok(None) => (
            true,
            json!({"status": "success", "duplicate": true, "eventId": event_id}),
            200,
        ),
        Err(e) => {
            tracing::error!("signal ingest failed: {e}");
            (
                false,
                json!({"status": "error", "message": "Could not store the signal."}),
                500,
            )
        }
    }
}
