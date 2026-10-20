//! Client keys over HTTP: the open API closes once a client exists, each key
//! reaches only the accounts its access allows, and keys outlive a restart
//! and an export.
use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
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

async fn upstream() -> Server {
    serve(
        Router::new()
            .route(
                "/v1/models",
                get(|| async { Json(json!({"data":[{"id":"chat-model"}]})) }),
            )
            .route(
                "/v1/chat/completions",
                post(|| async {
                    Json(json!({"id":"chat_1","object":"chat.completion","model":"chat-model",
                    "choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":3,"completion_tokens":1}}))
                }),
            ),
    )
    .await
}

struct Instance {
    state: Arc<AppState>,
    server: Server,
    http: reqwest::Client,
}

async fn instance(directory: &TempDir) -> Instance {
    let mut config = Config::default();
    config.server.auth_dir = directory.path().join("accounts");
    config.server.database = directory.path().join("calls.sqlite");
    let state = AppState::new(config, None).unwrap();
    let server = serve(douane::router(state.clone())).await;
    Instance {
        state,
        server,
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap(),
    }
}

impl Instance {
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        key: Option<&str>,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.server.url));
        if let Some(key) = key {
            request = request.bearer_auth(key);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn models(&self, key: Option<&str>) -> (u16, Vec<String>) {
        let (status, body) = self
            .send(reqwest::Method::GET, "/v1/models", key, None)
            .await;
        let ids = body["data"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| row["id"].as_str().unwrap().to_owned())
                    .collect()
            })
            .unwrap_or_default();
        (status, ids)
    }

    async fn chat(&self, key: Option<&str>, model: &str) -> u16 {
        self.send(
            reqwest::Method::POST,
            "/v1/chat/completions",
            key,
            Some(json!({"model":model,"messages":[{"role":"user","content":"hi"}]})),
        )
        .await
        .0
    }
}

fn endpoint(route: &str, base: &str) -> Value {
    json!({"type":"openai_compatible","route":route,"base_url":format!("{base}/v1"),"api_key":format!("key-{route}"),"dialect":"chat"})
}

#[tokio::test]
async fn a_client_key_reaches_only_the_accounts_its_access_allows() {
    let api = upstream().await;
    let directory = tempfile::tempdir().unwrap();
    let gateway = instance(&directory).await;
    for route in ["loop", "spare"] {
        gateway
            .state
            .accounts
            .import(endpoint(route, &api.url), Some(route.into()))
            .await
            .unwrap();
    }
    gateway.state.refresh_models().await;
    // No client yet: the API is open, placeholder keys included.
    assert_eq!(gateway.models(None).await.1.len(), 2);
    assert_eq!(
        gateway
            .chat(Some("boite-subscription-proxy"), "spare/chat-model")
            .await,
        200
    );

    let (status, created) = gateway
        .send(
            reqwest::Method::POST,
            "/admin/api/clients",
            None,
            Some(json!({"name":"Famille","access":{"all":false,"pools":["loop"]}})),
        )
        .await;
    assert_eq!(status, 201, "{created}");
    let key = created["key"].as_str().unwrap().to_owned();
    assert!(created["client"].get("key_hash").is_none());

    assert_eq!(gateway.models(None).await.0, 401);
    assert_eq!(
        gateway.models(Some("boite-subscription-proxy")).await.0,
        401
    );
    assert_eq!(
        gateway.models(Some(&key)).await,
        (200, vec!["loop/chat-model".to_owned()])
    );
    assert_eq!(gateway.chat(None, "loop/chat-model").await, 401);
    assert_eq!(gateway.chat(Some(&key), "loop/chat-model").await, 200);
    assert_eq!(gateway.chat(Some(&key), "spare/chat-model").await, 503);
    let (status, quotas) = gateway
        .send(reqwest::Method::GET, "/v1/quotas", None, None)
        .await;
    assert_eq!(status, 401, "{quotas}");

    // The ledger names the client of each call.
    gateway.state.ledger.flush().await;
    let calls = gateway.state.ledger.calls(10).await;
    let id = created["client"]["id"].as_str().unwrap();
    assert!(
        calls
            .iter()
            .any(|c| c.client_id.as_deref() == Some(id) && c.status == 200)
    );

    // The account folder still opens: client files are not accounts.
    assert_eq!(
        AccountStore::open(directory.path().join("accounts"))
            .unwrap()
            .list()
            .await
            .len(),
        2
    );

    // The key outlives a restart, and travels in an export.
    let (_, bundle) = gateway
        .send(reqwest::Method::GET, "/admin/api/export", None, None)
        .await;
    drop(gateway);
    let restarted = instance(&directory).await;
    restarted.state.refresh_models().await;
    assert_eq!(restarted.chat(Some(&key), "loop/chat-model").await, 200);

    let elsewhere = tempfile::tempdir().unwrap();
    let target = instance(&elsewhere).await;
    let (status, restored) = target
        .send(
            reqwest::Method::POST,
            "/admin/api/restore",
            None,
            Some(bundle),
        )
        .await;
    assert_eq!(status, 200, "{restored}");
    assert_eq!(restored["clients"], 1);
    assert_eq!(
        target.models(Some(&key)).await,
        (200, vec!["loop/chat-model".to_owned()])
    );

    // Deleting the last client opens the API again.
    let (status, _) = target
        .send(
            reqwest::Method::DELETE,
            &format!("/admin/api/clients/{id}"),
            None,
            None,
        )
        .await;
    assert_eq!(status, 204);
    assert_eq!(target.models(None).await.0, 200);
}
