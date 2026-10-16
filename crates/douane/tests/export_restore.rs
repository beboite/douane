//! Configuration export and restore between two instances, with synthetic
//! custom endpoints against a loopback upstream.
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::State,
    http::HeaderMap,
    routing::{get, post},
};
use douane::{settings::Config, state::AppState, subscriptions::AccountStore};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{net::TcpListener, task::JoinHandle};

struct Server {
    url: String,
    task: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(router: Router) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Server { url, task }
}

/// An OpenAI-compatible API serving one model and recording the keys it saw.
async fn upstream() -> (Server, Arc<Mutex<Vec<String>>>) {
    let keys = Arc::new(Mutex::new(vec![]));
    let server = serve(
        Router::new()
            .route(
                "/v1/models",
                get(|| async { Json(json!({"data":[{"id":"chat-model"}]})) }),
            )
            .route(
                "/v1/chat/completions",
                post(
                    |State(keys): State<Arc<Mutex<Vec<String>>>>, headers: HeaderMap| async move {
                        keys.lock().unwrap().push(
                            headers["authorization"].to_str().unwrap().to_owned(),
                        );
                        Json(json!({"id":"chat_1","object":"chat.completion","model":"chat-model",
                        "choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
                        "usage":{"prompt_tokens":3,"completion_tokens":1}}))
                    },
                ),
            )
            .with_state(keys.clone()),
    )
    .await;
    (server, keys)
}

struct Instance {
    state: Arc<AppState>,
    server: Server,
    client: reqwest::Client,
    directory: TempDir,
}

impl Instance {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.server.auth_dir = directory.path().join("accounts");
        config.server.database = directory.path().join("calls.sqlite");
        let path = directory.path().join("douane.toml");
        config.save(&path).await.unwrap();
        let state = AppState::new(config, Some(path)).unwrap();
        let server = serve(douane::router(state.clone())).await;
        Self {
            state,
            server,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            directory,
        }
    }

    async fn send(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.server.url));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        self.send(reqwest::Method::POST, path, Some(body)).await
    }
}

fn endpoint(route: &str, base: &str, key: &str) -> Value {
    json!({"type":"openai_compatible","route":route,"base_url":format!("{base}/v1"),"api_key":key,"dialect":"chat"})
}

#[tokio::test]
async fn an_export_restored_into_a_fresh_instance_serves_its_endpoints() {
    let (api, keys) = upstream().await;
    let source = Instance::new().await;
    let (status, imported) = source
        .post(
            "/admin/api/accounts/import",
            json!({"files":[
                {"name":"Loop","content":endpoint("loop", &api.url, "key-loop")},
                {"name":"Spare","content":endpoint("spare", &api.url, "key-spare")}
            ]}),
        )
        .await;
    assert_eq!(status, 200, "{imported}");
    let (status, _) = source
        .send(
            reqwest::Method::PATCH,
            "/admin/api/settings",
            Some(json!({"quota_refresh_seconds":120})),
        )
        .await;
    assert_eq!(status, 200);
    let (status, _) = source
        .send(
            reqwest::Method::PATCH,
            "/admin/api/models/spare/chat-model",
            Some(json!({"enabled":false})),
        )
        .await;
    assert_eq!(status, 200);

    let response = source
        .client
        .get(format!("{}/admin/api/export", source.server.url))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let disposition = response.headers()["content-disposition"].to_str().unwrap();
    assert!(
        disposition.starts_with("attachment; filename=\"douane-export-20"),
        "{disposition}"
    );
    let bundle: Value = response.json().await.unwrap();
    assert_eq!(bundle["format"], "douane-export");
    assert_eq!(bundle["version"], 1);
    assert_eq!(bundle["settings"]["quota_refresh_seconds"], 120);
    assert_eq!(bundle["disabled_models"], json!(["spare/chat-model"]));
    // The export carries the stored record, key included.
    let accounts = bundle["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 2);
    assert!(accounts.iter().any(|a| a["access_token"] == "key-loop"));

    let target = Instance::new().await;
    let (status, restored) = target
        .post(
            "/admin/api/restore",
            json!({"bundle":bundle,"settings":true}),
        )
        .await;
    assert_eq!(status, 200, "{restored}");
    assert_eq!(restored["restored"], 2);
    assert_eq!(restored["errors"], json!([]));
    assert_eq!(restored["settings"], true);
    assert!(!restored.to_string().contains("key-loop"));
    // Restoring the same file again updates the accounts instead of adding.
    let (status, again) = target.post("/admin/api/restore", bundle.clone()).await;
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["settings"], false);
    assert_eq!(target.state.accounts.list().await.len(), 2);

    let config = target.state.config.read().await.clone();
    assert_eq!(config.settings.quota_refresh_seconds, 120);
    assert!(config.disabled_models.contains("spare/chat-model"));
    // Settings reached the configuration file, accounts the auth folder.
    let saved = Config::load(&target.directory.path().join("douane.toml")).unwrap();
    assert_eq!(saved.settings.quota_refresh_seconds, 120);
    let stored = AccountStore::open(target.directory.path().join("accounts")).unwrap();
    let mut routes: Vec<_> = stored
        .list()
        .await
        .into_iter()
        .filter_map(|a| a.route)
        .collect();
    routes.sort();
    assert_eq!(routes, ["loop", "spare"]);

    let (status, answer) = target
        .post(
            "/v1/chat/completions",
            json!({"model":"loop/chat-model","messages":[{"role":"user","content":"hi"}]}),
        )
        .await;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["choices"][0]["message"]["content"], "ok");
    assert_eq!(*keys.lock().unwrap(), ["Bearer key-loop"]);
    // The disabled model stays disabled on the new instance.
    let (status, _) = target
        .post(
            "/v1/chat/completions",
            json!({"model":"spare/chat-model","messages":[{"role":"user","content":"hi"}]}),
        )
        .await;
    assert_ne!(status, 200);
}

#[tokio::test]
async fn malformed_bundles_are_refused_and_change_nothing() {
    let (api, _) = upstream().await;
    let instance = Instance::new().await;
    let good = json!({"format":"douane-export","version":1,"settings":{},"disabled_models":[],"accounts":[]});
    let with = |key: &str, value: Value| {
        let mut bundle = good.clone();
        bundle[key] = value;
        bundle
    };
    let cases = [
        (json!({"files":[]}), "format must be"),
        (with("format", json!("cliproxy")), "format must be"),
        (with("version", json!(2)), "unsupported export version 2"),
        (with("version", json!("1")), "unsupported export version"),
        (with("accounts", json!({})), "accounts must be a list"),
        (
            json!({"bundle":good,"settings":"yes"}),
            "must be true or false",
        ),
        (
            json!({"bundle":with("settings", json!({"quota_refresh_seconds":1})),"settings":true}),
            "quota_refresh_seconds",
        ),
        (
            json!({"bundle":with("settings", json!({"unknown":1})),"settings":true}),
            "invalid settings",
        ),
        (
            json!({"bundle":with("disabled_models", json!("all")),"settings":true}),
            "disabled_models",
        ),
    ];
    for (body, message) in cases {
        let (status, answer) = instance.post("/admin/api/restore", body.clone()).await;
        assert_eq!(status, 400, "{body}");
        let text = answer["error"]["message"].as_str().unwrap_or_default();
        assert!(text.contains(message), "{body}: {text}");
    }
    // Records that fail their checks are reported one by one.
    let record = |route: &str, base: &str| {
        json!({"id":"not-an-id","name":"Bad","provider":"openai_compatible","access_token":"key",
            "refresh_token":null,"id_token":null,"account_id":null,"base_url":base,"project_id":null,
            "email":null,"plan":null,"expires_at":null,"enabled":true,
            "created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","route":route})
    };
    let (status, answer) = instance
        .post(
            "/admin/api/restore",
            with(
                "accounts",
                json!([
                    record("Claude", &format!("{}/v1", api.url)),
                    record("fine", "ftp://example.invalid"),
                    {"provider":"nobody"}
                ]),
            ),
        )
        .await;
    assert_eq!(status, 400, "{answer}");
    assert_eq!(answer["restored"], 0);
    let errors = answer["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 3);
    assert!(errors[0]["error"].as_str().unwrap().contains("route"));
    assert!(errors[1]["error"].as_str().unwrap().contains("base URL"));
    assert!(
        errors[2]["error"]
            .as_str()
            .unwrap()
            .contains("invalid account record")
    );
    assert!(instance.state.accounts.list().await.is_empty());
    assert_eq!(
        instance
            .state
            .config
            .read()
            .await
            .settings
            .quota_refresh_seconds,
        300
    );
    // A valid record beside a bad one is restored, with a fresh local id.
    let (status, answer) = instance
        .post(
            "/admin/api/restore",
            with(
                "accounts",
                json!([
                    record("fine", &format!("{}/v1", api.url)),
                    record("Claude", "")
                ]),
            ),
        )
        .await;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["restored"], 1);
    assert_eq!(answer["errors"].as_array().unwrap().len(), 1);
    let id = answer["accounts"][0]["id"].as_str().unwrap();
    assert_eq!(id.len(), 48);
}

#[tokio::test]
async fn a_restored_sign_in_replaces_the_same_identity() {
    let directory = tempfile::tempdir().unwrap();
    let store = AccountStore::open(directory.path().join("accounts")).unwrap();
    let local = store
        .import(
            json!({"type":"claude","access_token":"old-token","refresh_token":"old-refresh",
                "account_id":"fixture-identity","name":"Local"}),
            None,
        )
        .await
        .unwrap();
    let other = tempfile::tempdir().unwrap();
    let remote = AccountStore::open(other.path().join("accounts")).unwrap();
    remote
        .import(
            json!({"type":"claude","access_token":"new-token","refresh_token":"new-refresh",
                "account_id":"fixture-identity","name":"Remote"}),
            None,
        )
        .await
        .unwrap();
    let record = serde_json::to_value(&remote.export().await[0]).unwrap();
    let restored = store.restore(record).await.unwrap();
    assert_eq!(restored.id, local.id);
    assert_eq!(restored.name, "Remote");
    let accounts = store.export().await;
    assert_eq!(accounts.len(), 1);
    let stored = serde_json::to_value(&accounts[0]).unwrap();
    assert_eq!(stored["access_token"], "new-token");
    assert_eq!(stored["refresh_token"], "new-refresh");
    // The change survives a reopen of the folder.
    let reopened = AccountStore::open(directory.path().join("accounts")).unwrap();
    assert_eq!(reopened.list().await[0].name, "Remote");
}
