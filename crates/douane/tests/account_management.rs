use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use douane::{settings::Config, state::AppState, subscriptions::AccountStore};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

#[tokio::test]
async fn rename_is_persistent_and_invalid_patches_do_not_partially_apply() {
    let directory = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.auth_dir = directory.path().join("auth");
    config.server.database = directory.path().join("requests.sqlite");
    let state = AppState::new(config.clone(), None).unwrap();
    let account = state
        .accounts
        .import(
            json!({"type":"claude","access_token":"fixture-token","name":"Original"}),
            None,
        )
        .await
        .unwrap();
    let app = douane::router(state.clone());
    let path = format!("/admin/api/accounts/{}", account.id);
    let patch = |value: Value| {
        Request::builder()
            .method("PATCH")
            .uri(&path)
            .header("content-type", "application/json")
            .body(Body::from(value.to_string()))
            .unwrap()
    };

    let response = app
        .clone()
        .oneshot(patch(json!({"name":"  Travail  "})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["account"]["name"], "Travail");
    assert_eq!(body["account"]["enabled"], true);
    assert!(!body.to_string().contains("fixture-token"));
    assert_eq!(
        AccountStore::open(&config.server.auth_dir)
            .unwrap()
            .list()
            .await[0]
            .name,
        "Travail"
    );

    for name in [
        String::new(),
        "   ".into(),
        "bad\nname".into(),
        "a".repeat(121),
    ] {
        let response = app
            .clone()
            .oneshot(patch(json!({"name":name,"enabled":false})))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let current = state.accounts.list().await.pop().unwrap();
        assert!(current.enabled);
        assert_eq!(current.name, "Travail");
    }
    let response = app
        .clone()
        .oneshot(patch(json!({"name":"Archive","enabled":false})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let current = AccountStore::open(&config.server.auth_dir)
        .unwrap()
        .list()
        .await
        .pop()
        .unwrap();
    assert_eq!(current.name, "Archive");
    assert!(!current.enabled);
}

#[tokio::test]
async fn reset_redemption_requires_explicit_confirmation_and_a_valid_request_id() {
    let directory = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.auth_dir = directory.path().join("auth");
    config.server.database = directory.path().join("requests.sqlite");
    let state = AppState::new(config, None).unwrap();
    let app = douane::router(state);
    for value in [
        json!({"request_id":"d5438ea2-f6a4-40a3-a2c5-b8bce88169d3"}),
        json!({"confirmation":false,"request_id":"d5438ea2-f6a4-40a3-a2c5-b8bce88169d3"}),
        json!({"confirmation":true,"request_id":"invalid"}),
    ] {
        let request = Request::builder()
            .method("POST")
            .uri("/admin/api/accounts/unknown/resets/redeem")
            .header("content-type", "application/json")
            .body(Body::from(value.to_string()))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert_eq!(
        std::fs::read_dir(directory.path().join("auth"))
            .unwrap()
            .count(),
        0
    );
}
