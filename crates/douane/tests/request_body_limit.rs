//! The relay must accept request bodies up to the documented limit.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use douane::{settings::Config, state::AppState};
use tower::ServiceExt;

async fn post_messages(size: usize) -> StatusCode {
    let directory = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.server.auth_dir = directory.path().join("auth");
    config.server.database = directory.path().join("requests.sqlite");
    let state = AppState::new(config, None).unwrap();
    douane::router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .header("content-length", size)
                .body(Body::from(vec![b'a'; size]))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn bodies_past_two_mebibytes_reach_the_relay() {
    // Invalid JSON reaches the handler and fails there; the body is not refused.
    assert_eq!(
        post_messages(3 * 1024 * 1024).await,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn bodies_past_the_limit_are_refused() {
    assert_eq!(
        post_messages(douane::MAX_REQUEST_BODY + 1).await,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}
