//! End-to-end checks for the ported OI routes: the server is built with
//! `server::app(ctx)` and driven through `oneshot`, the same harness every
//! other route test uses. These need neither a broker nor the network -- with
//! the recorder off and no store opened, the Writer Flow routes answer
//! "empty"/"success" rather than 404 or a panic, which proves the handlers,
//! their services, and the route registrations are wired end to end.

use super::*;

/// A signed-in GET.
fn get_signed(h: &H, path: &str) -> Request<Body> {
    let (cookie, _) = h.session(true);
    with_session(get(path), &cookie, None)
}

#[tokio::test]
async fn writerflow_sessions_answers_success_with_the_recorder_off() {
    let h = H::new();
    h.setup();
    let (s, v) = h.json(get_signed(&h, "/writerflow/api/sessions")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], json!("success"), "{v}");
    // No store opened in a test, so the recorder reads as off and nothing is listed.
    assert_eq!(v["recorder_enabled"], json!(false), "{v}");
    assert_eq!(v["sessions"], json!([]), "{v}");
}

#[tokio::test]
async fn writerflow_flow_answers_empty_without_a_recording() {
    let h = H::new();
    h.setup();
    let (s, v) = h.json(get_signed(&h, "/writerflow/api/flow?underlying=NIFTY")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], json!("empty"), "{v}");
}

#[tokio::test]
async fn writerflow_flow_refuses_a_missing_underlying() {
    let h = H::new();
    h.setup();
    let (s, v) = h.json(get_signed(&h, "/writerflow/api/flow")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], json!("error"), "{v}");
}

#[tokio::test]
async fn writerflow_levels_answers_empty_without_a_recording() {
    let h = H::new();
    h.setup();
    let (s, v) = h.json(get_signed(&h, "/writerflow/api/levels?underlying=NIFTY")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], json!("empty"), "{v}");
}
