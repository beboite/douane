use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use douane::{
    settings::{Config, QuotaDisplay},
    state::AppState,
    subscriptions::SubscriptionProvider,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

fn state(directory: &std::path::Path, path: Option<std::path::PathBuf>) -> Arc<AppState> {
    let mut config = Config::default();
    config.server.auth_dir = directory.join("auth");
    config.server.database = directory.join("requests.sqlite");
    let mut state = AppState::new(config, path).unwrap();
    Arc::get_mut(&mut state).unwrap().management_token = Some("fixture-management".into());
    state
}

async fn send(app: &axum::Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

fn get(path: &str, bearer: Option<&str>) -> Request<Body> {
    let request = Request::builder().uri(path);
    match bearer {
        Some(bearer) => request.header("authorization", format!("Bearer {bearer}")),
        None => request,
    }
    .body(Body::empty())
    .unwrap()
}

fn patch_settings(value: Value) -> Request<Body> {
    Request::builder()
        .method("PATCH")
        .uri("/admin/api/settings")
        .header("authorization", "Bearer fixture-management")
        .header("content-type", "application/json")
        .body(Body::from(value.to_string()))
        .unwrap()
}

#[tokio::test]
async fn quotas_use_the_same_client_authentication_as_models() {
    let directory = tempfile::tempdir().unwrap();
    let state = state(directory.path(), None);
    state
        .accounts
        .import(
            json!({"type":"claude","access_token":"fixture-access","refresh_token":"fixture-refresh","name":"chris@example.com"}),
            None,
        )
        .await
        .unwrap();
    let app = douane::router(state);
    for bearer in [None, Some("douane"), Some("subscription-proxy-key")] {
        let (models, _) = send(&app, get("/v1/models", bearer)).await;
        let (status, body) = send(&app, get("/v1/quotas", bearer)).await;
        assert_eq!(status, models);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "quotas");
        let entry = &body["providers"][0]["entries"][0];
        assert_eq!(entry["label"], "chris@…");
        assert_eq!(entry["accounts"], 1);
        assert_eq!(entry["status"], "ready");
        for field in ["plan", "error", "updated_at"] {
            assert_eq!(entry.get(field), Some(&Value::Null));
        }
        let text = body.to_string();
        assert!(!text.contains("fixture-"));
        assert!(!text.contains("example.com"));
    }
    // Management credentials are not client credentials and do not change the answer.
    let (status, _) = send(&app, get("/admin/api/quotas", Some("douane"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_client_refresh_reads_every_account_and_reports_failures_at_once() {
    let directory = tempfile::tempdir().unwrap();
    let state = state(directory.path(), None);
    // Qwen has no quota reader: its read fails without leaving the process.
    state
        .accounts
        .import(
            json!({"type":"qwen","access_token":"fixture-access","name":"Qwen"}),
            None,
        )
        .await
        .unwrap();
    let app = douane::router(state);
    let (_, body) = send(&app, get("/v1/quotas", None)).await;
    assert_eq!(body["providers"][0]["entries"][0]["error"], Value::Null);
    let (status, body) = send(&app, get("/v1/quotas?refresh=1", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["providers"][0]["entries"][0]["error"],
        "provider does not support this operation"
    );
}

#[tokio::test]
async fn quota_display_defaults_to_accounts_for_every_provider() {
    let directory = tempfile::tempdir().unwrap();
    let state = state(directory.path(), None);
    for name in ["One", "Two"] {
        state
            .accounts
            .import(
                json!({"type":"codex","access_token":"fixture-access","name":name}),
                None,
            )
            .await
            .unwrap();
    }
    let app = douane::router(state.clone());
    let (_, settings) = send(&app, get("/admin/api/settings", Some("fixture-management"))).await;
    assert_eq!(settings["quota_display"], json!({}));
    assert_eq!(
        state
            .config
            .read()
            .await
            .settings
            .quota_display(SubscriptionProvider::Codex),
        QuotaDisplay::Accounts
    );
    let (_, body) = send(&app, get("/v1/quotas", None)).await;
    assert_eq!(body["providers"].as_array().unwrap().len(), 1);
    assert_eq!(body["providers"][0]["provider"], "codex");
    assert_eq!(body["providers"][0]["name"], "Codex");
    assert_eq!(body["providers"][0]["display"], "accounts");
    let labels: Vec<_> = body["providers"][0]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["One", "Two"]);
}

#[tokio::test]
async fn quota_display_persists_through_the_settings_api() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("douane.toml");
    let state = state(directory.path(), Some(path.clone()));
    for name in ["One", "Two"] {
        state
            .accounts
            .import(
                json!({"type":"antigravity","access_token":"fixture-access","name":name}),
                None,
            )
            .await
            .unwrap();
    }
    let app = douane::router(state);

    let (status, settings) = send(
        &app,
        patch_settings(json!({"quota_display":{"antigravity":"average"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(settings["quota_display"], json!({"antigravity":"average"}));
    assert_eq!(settings["strategy"], "smart");

    let saved = Config::load(&path).unwrap();
    assert_eq!(
        saved
            .settings
            .quota_display(SubscriptionProvider::Antigravity),
        QuotaDisplay::Average
    );
    assert_eq!(
        saved.settings.quota_display(SubscriptionProvider::Claude),
        QuotaDisplay::Accounts
    );

    let (_, body) = send(&app, get("/v1/quotas", None)).await;
    let provider = &body["providers"][0];
    assert_eq!(provider["display"], "average");
    assert_eq!(provider["entries"].as_array().unwrap().len(), 1);
    assert_eq!(provider["entries"][0]["id"], "antigravity");
    assert_eq!(provider["entries"][0]["label"], "Antigravity · 2 comptes");
    assert_eq!(provider["entries"][0]["accounts"], 2);

    for invalid in [
        json!({"quota_display":{"anthropic":"average"}}),
        json!({"quota_display":{"unknown":"average"}}),
        json!({"quota_display":{"claude":"sum"}}),
    ] {
        let (status, _) = send(&app, patch_settings(invalid)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (_, settings) = send(&app, get("/admin/api/settings", Some("fixture-management"))).await;
    assert_eq!(settings["quota_display"], json!({"antigravity":"average"}));

    let (status, settings) = send(&app, patch_settings(json!({"quota_display":{}}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(settings["quota_display"], json!({}));
    let (_, body) = send(&app, get("/v1/quotas", None)).await;
    assert_eq!(body["providers"][0]["display"], "accounts");
    assert_eq!(body["providers"][0]["entries"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn quota_windows_persist_and_reject_bad_ids() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("douane.toml");
    let app = douane::router(state(directory.path(), Some(path.clone())));
    let choice = json!({"claude":{"hidden":["seven-day-opus"],"primary":"five-hour"}});

    let (status, settings) = send(&app, patch_settings(json!({ "quota_windows": choice }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(settings["quota_windows"], choice);
    let saved = Config::load(&path).unwrap();
    let windows = saved
        .settings
        .quota_windows(SubscriptionProvider::Claude)
        .unwrap();
    assert_eq!(windows.primary.as_deref(), Some("five-hour"));
    assert!(windows.hidden.contains("seven-day-opus"));
    assert!(
        saved
            .settings
            .quota_windows(SubscriptionProvider::Codex)
            .is_none()
    );

    let long = "x".repeat(81);
    for invalid in [
        json!({"quota_windows":{"anthropic":{}}}),
        json!({"quota_windows":{"claude":{"hidden":[""]}}}),
        json!({"quota_windows":{"claude":{"hidden":[long]}}}),
        json!({"quota_windows":{"claude":{"primary":"a\nb"}}}),
        json!({"quota_windows":{"claude":{"shown":["five-hour"]}}}),
    ] {
        let (status, _) = send(&app, patch_settings(invalid)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (_, settings) = send(&app, get("/admin/api/settings", Some("fixture-management"))).await;
    assert_eq!(settings["quota_windows"], choice);

    let (status, settings) = send(&app, patch_settings(json!({"quota_windows":{}}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(settings["quota_windows"], json!({}));
}
