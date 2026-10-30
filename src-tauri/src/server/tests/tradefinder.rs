//! End-to-end checks for the TradeFinder proxy endpoints. With no server-side
//! TradeFinder token configured (the test's temp data_dir has none, and no env
//! token is set), the service returns before any network call, so these run
//! fully offline: a valid apikey gets a 200 `status: error` with a trader-facing
//! "no session configured" message (never a hard failure, so the client can
//! fall back), and a wrong apikey is refused outright. Boots the real server
//! in-process; no broker or network needed.

use super::*;

#[tokio::test]
async fn market_pulse_without_a_token_is_a_soft_error_naming_the_setup() {
    let h = H::new();
    let key = h.setup();

    let (s, v) = h
        .json(post_json("/api/v1/tfmarketpulse", json!({"apikey": key})))
        .await;
    // Soft error: HTTP 200 so the client treats it as "fall back", not a crash.
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], json!("error"), "{v}");
    let msg = v["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("TradeFinder") && msg.contains("token"),
        "message should name the cause and the fix, got: {msg:?}"
    );
    // No status codes or protocol terms leaked to the trader.
    assert!(!msg.contains("500") && !msg.contains("HTTP"), "{msg:?}");
}

#[tokio::test]
async fn sector_scope_without_a_token_is_a_soft_error() {
    let h = H::new();
    let key = h.setup();

    let (s, v) = h
        .json(post_json("/api/v1/tfsectorscope", json!({"apikey": key})))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], json!("error"), "{v}");
    assert!(v["message"].as_str().is_some(), "{v}");
}

#[tokio::test]
async fn market_pulse_refuses_a_wrong_apikey() {
    let h = H::new();
    h.setup();

    let (s, _v) = h
        .json(post_json("/api/v1/tfmarketpulse", json!({"apikey": "wrong-key"})))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn jwt_keepalive_reports_no_token_when_none_is_configured() {
    let h = H::new();
    let key = h.setup();
    let (s, v) = h
        .json(post_json(
            "/api/v1/tfjwtkeepalive",
            json!({"apikey": key, "minSeconds": 1800}),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], json!("success"), "{v}");
    assert_eq!(v["hasToken"], json!(false), "{v}");
    assert_eq!(v["refreshing"], json!(false), "{v}");
}

#[tokio::test]
async fn background_setting_defaults_off_then_persists_a_toggle() {
    let h = H::new();
    h.setup();
    let (cookie, csrf) = h.session(true);

    // Defaults off.
    let (s, v) = h
        .json(with_session(get("/trading/tradefinder/settings"), &cookie, None))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["background_enabled"], json!(false), "{v}");

    // Turning it on persists through the store.
    let (s2, v2) = h
        .json(with_session(
            post_json("/trading/tradefinder/settings", json!({"background_enabled": true})),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s2, StatusCode::OK, "{v2}");
    assert_eq!(v2["background_enabled"], json!(true), "{v2}");

    let (s3, v3) = h
        .json(with_session(get("/trading/tradefinder/settings"), &cookie, None))
        .await;
    assert_eq!(s3, StatusCode::OK, "{v3}");
    assert_eq!(v3["background_enabled"], json!(true), "{v3}");
}
