use axum::{body::Body, http::Request};
use douane::{settings::Config, state::AppState};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

fn configured(directory: &std::path::Path, base: &str) -> anyhow::Result<Config> {
    let path = directory.join("douane.toml");
    let base = toml::Value::String(base.into());
    std::fs::write(
        &path,
        format!(
            "[server]\nauth_dir = 'auth'\ndatabase = 'requests.sqlite'\nclient_api_base_url = {base}\n"
        ),
    )?;
    Config::load(&path)
}

#[test]
fn client_api_base_url_configuration_accepts_supported_bases() {
    let directory = tempfile::tempdir().unwrap();
    for (base, expected) in [
        ("http://127.0.0.1:8787", "http://127.0.0.1:8787/v1"),
        ("https://example.invalid/v1/", "https://example.invalid/v1"),
        (
            "https://example.invalid/proxy/",
            "https://example.invalid/proxy/v1",
        ),
        (
            "https://example.invalid/proxy/v1",
            "https://example.invalid/proxy/v1",
        ),
        ("http://[::1]:8787", "http://[::1]:8787/v1"),
        (
            "https://example.invalid/proxy%20path",
            "https://example.invalid/proxy%20path/v1",
        ),
    ] {
        let config =
            configured(directory.path(), base).expect("a client API address must be accepted");
        assert_eq!(config.server.client_api_base_url.as_deref(), Some(expected));
    }
}

#[test]
fn client_api_base_url_configuration_rejects_non_public_url_parts() {
    let directory = tempfile::tempdir().unwrap();
    for base in [
        "ftp://example.invalid/v1",
        "//example.invalid/v1",
        "/v1",
        "https://fixture-user:fixture-private@example.invalid/v1",
        "https://example.invalid/v1?key=fixture-private",
        "https://example.invalid/v1#fixture-private",
        "https://@example.invalid/v1",
        "https://example.invalid/v1?",
        "https://example.invalid/v1#",
        " https://example.invalid/v1",
        "https://example.invalid/v1 ",
        "https://example.invalid/proxy path/v1",
        "https://example.invalid/v1\tfixture-private",
        "https://example.invalid/v1\nfixture-private",
        "https://example.invalid/v1\rfixture-private",
        "https://example.invalid/v1\u{7f}fixture-private",
    ] {
        let error = configured(directory.path(), base).unwrap_err().to_string();
        assert!(error.contains("client_api_base_url"));
        assert!(!error.contains("fixture-private"));
    }
}

#[test]
fn client_api_base_url_runtime_validation_precedes_data_creation() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.server.auth_dir = directory.path().join("auth");
    config.server.database = directory.path().join("requests.sqlite");
    config.server.client_api_base_url =
        Some("https://example.invalid/v1?key=fixture-private".into());
    let error = match AppState::new(config, None) {
        Ok(_) => panic!("a private client API address must be rejected"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("client_api_base_url"));
    assert!(!error.contains("fixture-private"));
    assert!(!directory.path().join("auth").exists());
    assert!(!directory.path().join("requests.sqlite").exists());
}

#[tokio::test]
async fn client_api_base_url_summary_preserves_paths_and_defaults_to_same_origin() {
    let directory = tempfile::tempdir().unwrap();
    let mut default = Config::default();
    default.server.auth_dir = directory.path().join("default-auth");
    default.server.database = directory.path().join("default.sqlite");
    let state = AppState::new(default, None).unwrap();
    let response = douane::router(state)
        .oneshot(
            Request::builder()
                .uri("/admin/api/summary")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let summary: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(summary.get("client_api_base_url"), Some(&Value::Null));

    for (base, expected) in [
        ("http://127.0.0.1:8787", "http://127.0.0.1:8787/v1"),
        ("https://example.invalid/v1/", "https://example.invalid/v1"),
        (
            "https://example.invalid/proxy/",
            "https://example.invalid/proxy/v1",
        ),
        (
            "https://example.invalid/proxy/v1",
            "https://example.invalid/proxy/v1",
        ),
    ] {
        let config = configured(directory.path(), base).unwrap();
        let state = AppState::new(config, None).unwrap();
        let response = douane::router(state)
            .oneshot(
                Request::builder()
                    .uri("/admin/api/summary")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let summary: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(summary["client_api_base_url"], expected);
    }
}
