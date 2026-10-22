//! Custom API endpoints and GitHub Copilot, against loopback upstreams.
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{OriginalUri, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use douane::{settings::Config, state::AppState};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{net::TcpListener, task::JoinHandle};

#[derive(Clone)]
struct Seen {
    path: String,
    headers: HeaderMap,
    body: Value,
}

#[derive(Default)]
struct Upstream {
    seen: Mutex<Vec<Seen>>,
}

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

/// Answers each wire format the way its provider does, and records what
/// douane sent.
async fn answer(
    State(upstream): State<Arc<Upstream>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap();
    let path = uri.path().to_owned();
    upstream.seen.lock().unwrap().push(Seen {
        path: path.clone(),
        headers,
        body: body.clone(),
    });
    let model = body["model"].clone();
    // Copilot lists models a plan cannot use, then refuses them.
    if model == "kimi-k3" {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                json!({"error":{"message":"The requested model is not supported.",
                "code":"model_not_supported","param":"model","type":"invalid_request_error"}}),
            ),
        )
            .into_response();
    }
    Json(if path.ends_with("/messages") {
        json!({"id":"msg_1","type":"message","role":"assistant","model":model,
            "content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn",
            "usage":{"input_tokens":3,"output_tokens":1}})
    } else if path.ends_with("/responses") {
        json!({"id":"resp_1","object":"response","status":"completed","model":model,
            "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}],
            "usage":{"input_tokens":3,"output_tokens":1}})
    } else {
        json!({"id":"chat_1","object":"chat.completion","model":model,
            "choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":3,"completion_tokens":1}})
    })
    .into_response()
}

/// An OpenAI-compatible API listing `models` under `/v1/models`.
async fn api(models: &'static [&'static str]) -> (Server, Arc<Upstream>) {
    let upstream = Arc::new(Upstream::default());
    let rows: Vec<Value> = models.iter().map(|id| json!({"id":id})).collect();
    let server = serve(
        Router::new()
            .route(
                "/v1/models",
                get(move || {
                    let rows = rows.clone();
                    async move { Json(json!({ "data": rows })) }
                }),
            )
            .route("/v1/chat/completions", post(answer))
            .route("/v1/responses", post(answer))
            .route("/v1/messages", post(answer))
            .with_state(upstream.clone()),
    )
    .await;
    (server, upstream)
}

/// Copilot's API: its catalog with policies and paths, and the three paths.
async fn copilot() -> (Server, Arc<Upstream>) {
    let upstream = Arc::new(Upstream::default());
    let server = serve(
        Router::new()
            .route(
                "/models",
                get(|| async {
                    Json(json!({"data":[
                        {"id":"claude-haiku-4.5","name":"Claude Haiku 4.5","policy":{"state":"enabled"},
                         "supported_endpoints":["/chat/completions","/v1/messages"],
                         "capabilities":{"type":"chat","limits":{"max_context_window_tokens":144000,"max_output_tokens":32000},"supports":{}}},
                        {"id":"gpt-6-luna","name":"GPT-6 Luna","policy":{"state":"enabled"},
                         "supported_endpoints":["/responses","ws:/responses"],
                         "capabilities":{"type":"chat","limits":{"max_context_window_tokens":1000000},"supports":{"reasoning_effort":["low","high"]}}},
                        {"id":"gpt-4.1","name":"GPT-4.1","policy":{"state":"enabled"},
                         "capabilities":{"type":"chat","limits":{"max_prompt_tokens":128000},"supports":{}}},
                        {"id":"claude-opus-5.5","name":"Claude Opus 5.5","policy":{"state":"disabled"},
                         "supported_endpoints":["/v1/messages"],"capabilities":{"type":"chat","supports":{}}},
                        {"id":"kimi-k3","name":"Kimi K3","policy":{"state":"enabled"},
                         "supported_endpoints":["/chat/completions"],"capabilities":{"type":"chat","supports":{}}},
                        {"id":"text-embedding-3-small","policy":{"state":"enabled"},"capabilities":{"type":"embeddings"}}
                    ]}))
                }),
            )
            .route("/chat/completions", post(answer))
            .route("/responses", post(answer))
            .route("/v1/messages", post(answer))
            .with_state(upstream.clone()),
    )
    .await;
    (server, upstream)
}

struct Gateway {
    state: Arc<AppState>,
    server: Server,
    client: reqwest::Client,
    _directory: TempDir,
}

impl Gateway {
    async fn new(accounts: &[Value]) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.server.auth_dir = directory.path().join("accounts");
        config.server.database = directory.path().join("calls.sqlite");
        let state = AppState::new(config, None).unwrap();
        let (_, key) = state
            .clients
            .create(douane::clients::ClientPatch {
                name: Some("tests".into()),
                ..Default::default()
            })
            .unwrap();
        for account in accounts {
            state.accounts.import(account.clone(), None).await.unwrap();
        }
        state.refresh_models().await;
        let server = serve(douane::router(state.clone())).await;
        Self {
            state,
            server,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .default_headers(
                    [(
                        reqwest::header::AUTHORIZATION,
                        format!("Bearer {key}").parse().unwrap(),
                    )]
                    .into_iter()
                    .collect(),
                )
                .build()
                .unwrap(),
            _directory: directory,
        }
    }

    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let response = self
            .client
            .post(format!("{}{path}", self.server.url))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn admin(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Value {
        let mut request = self
            .client
            .request(method, format!("{}/admin/api{path}", self.server.url));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        assert!(
            response.status().is_success(),
            "{path}: {}",
            response.status()
        );
        response.json().await.unwrap()
    }

    async fn model_ids(&self) -> Vec<String> {
        let models: Value = self
            .client
            .get(format!("{}/v1/models", self.server.url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let mut ids: Vec<String> = models["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_owned())
            .collect();
        ids.sort();
        ids
    }
}

fn endpoint(route: &str, base: &str, key: &str) -> Value {
    json!({"type":"openai_compatible","route":route,"base_url":format!("{base}/v1"),"api_key":key,"dialect":"chat"})
}

fn chat(model: &str) -> Value {
    json!({"model":model,"messages":[{"role":"user","content":"hi"}]})
}

#[tokio::test]
async fn each_route_keeps_its_own_keys_and_address() {
    let (deepseek, deepseek_seen) = api(&["shared-model", "deepseek-chat"]).await;
    let (nvidia, nvidia_seen) = api(&["shared-model", "nemotron"]).await;
    let gateway = Gateway::new(&[
        endpoint("deepseek", &deepseek.url, "key-deepseek"),
        endpoint("nvidia", &nvidia.url, "key-nvidia"),
    ])
    .await;
    assert_eq!(
        gateway.model_ids().await,
        [
            "deepseek/deepseek-chat",
            "deepseek/shared-model",
            "nvidia/nemotron",
            "nvidia/shared-model"
        ]
    );
    // Both keys accept every model, yet a route never borrows the other's key.
    for _ in 0..4 {
        let (status, _) = gateway
            .post("/v1/chat/completions", chat("deepseek/shared-model"))
            .await;
        assert_eq!(status, 200);
    }
    let (status, _) = gateway
        .post("/v1/chat/completions", chat("nvidia/shared-model"))
        .await;
    assert_eq!(status, 200);
    let deepseek_calls = deepseek_seen.seen.lock().unwrap().clone();
    let nvidia_calls = nvidia_seen.seen.lock().unwrap().clone();
    assert_eq!(deepseek_calls.len(), 4);
    assert_eq!(nvidia_calls.len(), 1);
    for call in &deepseek_calls {
        assert_eq!(call.headers["authorization"], "Bearer key-deepseek");
        assert_eq!(call.body["model"], "shared-model");
    }
    assert_eq!(
        nvidia_calls[0].headers["authorization"],
        "Bearer key-nvidia"
    );
    // A disabled key takes its route out of the list, not the other route.
    let accounts = gateway.admin(reqwest::Method::GET, "/accounts", None).await;
    let nvidia_id = accounts["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["route"] == "nvidia")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    gateway
        .admin(
            reqwest::Method::PATCH,
            &format!("/accounts/{nvidia_id}"),
            Some(json!({"enabled":false})),
        )
        .await;
    assert_eq!(
        gateway.model_ids().await,
        ["deepseek/deepseek-chat", "deepseek/shared-model"]
    );
    // A bare model id still finds its only route.
    let (status, _) = gateway
        .post("/v1/chat/completions", chat("deepseek-chat"))
        .await;
    assert_eq!(status, 200);
    assert_eq!(deepseek_seen.seen.lock().unwrap().len(), 5);
}

#[tokio::test]
async fn a_chat_endpoint_serves_messages_and_moves_with_its_address() {
    let (first, first_seen) = api(&["glm-5"]).await;
    let (second, second_seen) = api(&["glm-5"]).await;
    let gateway = Gateway::new(&[endpoint("zai", &first.url, "key-zai")]).await;
    // A Messages client reaches a Chat Completions endpoint through translation.
    let (status, body) = gateway
        .post(
            "/v1/messages",
            json!({"model":"zai/glm-5","max_tokens":32,"messages":[{"role":"user","content":"hi"}]}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["content"][0]["text"], "ok");
    assert_eq!(
        first_seen.seen.lock().unwrap()[0].path,
        "/v1/chat/completions"
    );
    // Moving the endpoint keeps its key and its route.
    let accounts = gateway.admin(reqwest::Method::GET, "/accounts", None).await;
    let row = &accounts["accounts"][0];
    assert_eq!(row["route"], "zai");
    assert_eq!(row["dialect"], "chat");
    assert!(
        !accounts.to_string().contains("key-zai"),
        "the key never comes back"
    );
    let id = row["id"].as_str().unwrap();
    let changed = gateway
        .admin(
            reqwest::Method::PATCH,
            &format!("/accounts/{id}"),
            Some(json!({"base_url":format!("{}/v1", second.url)})),
        )
        .await;
    assert_eq!(changed["account"]["base_url"], format!("{}/v1", second.url));
    let (status, _) = gateway
        .post("/v1/chat/completions", chat("zai/glm-5"))
        .await;
    assert_eq!(status, 200);
    let moved = second_seen.seen.lock().unwrap().clone();
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].headers["authorization"], "Bearer key-zai");
    // A refused change leaves the endpoint as it was.
    let refused = gateway
        .client
        .patch(format!("{}/admin/api/accounts/{id}", gateway.server.url))
        .json(&json!({"route":"claude","base_url":"https://elsewhere.test/v1"}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 400);
    let after = gateway.admin(reqwest::Method::GET, "/accounts", None).await;
    assert_eq!(after["accounts"][0]["route"], "zai");
    assert_eq!(
        after["accounts"][0]["base_url"],
        format!("{}/v1", second.url)
    );
}

#[tokio::test]
async fn an_endpoint_route_must_not_shadow_a_provider() {
    let gateway = Gateway::new(&[]).await;
    let long = "r".repeat(33);
    for route in [
        "claude",
        "openai",
        "copilot",
        "Deep Seek",
        "-x",
        long.as_str(),
    ] {
        let result = gateway
            .state
            .accounts
            .import(endpoint(route, "https://api.example.test", "key"), None)
            .await;
        assert!(result.is_err(), "{route:?} was accepted");
    }
}

#[tokio::test]
async fn a_failed_listing_keeps_the_previous_catalog() {
    // OpenRouter's listing once timed out during a refresh and its whole
    // pool vanished from /v1/models until douane restarted.
    let failing = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let switch = failing.clone();
    let server = serve(Router::new().route(
        "/v1/models",
        get(move || {
            let failing = switch.load(std::sync::atomic::Ordering::Relaxed);
            async move {
                if failing {
                    StatusCode::BAD_GATEWAY.into_response()
                } else {
                    Json(json!({"data":[{"id":"deepseek/deepseek-v4.1-flash"}]})).into_response()
                }
            }
        }),
    ))
    .await;
    let gateway = Gateway::new(&[endpoint("openrouter", &server.url, "key")]).await;
    let listed = ["openrouter/deepseek/deepseek-v4.1-flash"];
    assert_eq!(gateway.model_ids().await, listed);

    failing.store(true, std::sync::atomic::Ordering::Relaxed);
    gateway.state.refresh_models().await;
    assert_eq!(gateway.model_ids().await, listed);
    assert_eq!(gateway.state.stale_pools(), ["openrouter"]);

    failing.store(false, std::sync::atomic::Ordering::Relaxed);
    gateway.state.refresh_models().await;
    assert_eq!(gateway.model_ids().await, listed);
    assert!(gateway.state.stale_pools().is_empty());
}

#[tokio::test]
async fn a_pool_refresh_rereads_only_that_pool() {
    // The Models page refreshes one provider at a time: the others keep
    // their rows even when their own listing would now answer differently.
    let first = Arc::new(Mutex::new(vec!["a"]));
    let second = Arc::new(Mutex::new(vec!["x"]));
    let listing = |ids: Arc<Mutex<Vec<&'static str>>>| {
        Router::new().route(
            "/v1/models",
            get(move || {
                let data: Vec<Value> = ids
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|id| json!({"id":id}))
                    .collect();
                async move { Json(json!({ "data": data })) }
            }),
        )
    };
    let one = serve(listing(first.clone())).await;
    let two = serve(listing(second.clone())).await;
    let gateway = Gateway::new(&[
        endpoint("one", &one.url, "key"),
        endpoint("two", &two.url, "key"),
    ])
    .await;
    assert_eq!(gateway.model_ids().await, ["one/a", "two/x"]);
    let before = gateway.admin(reqwest::Method::GET, "/models", None).await;
    assert!(before["listings"]["one"]["refreshed_at"].is_string());
    assert_eq!(before["listings"]["two"]["stale"], false);

    first.lock().unwrap().push("b");
    second.lock().unwrap().push("y");
    let after = gateway
        .admin(reqwest::Method::POST, "/models/refresh?pool=one", None)
        .await;
    assert_eq!(after["models"].as_array().unwrap().len(), 3);
    assert_eq!(gateway.model_ids().await, ["one/a", "one/b", "two/x"]);
    assert!(after["listings"]["two"].is_object());

    gateway
        .admin(reqwest::Method::POST, "/models/refresh", None)
        .await;
    assert_eq!(
        gateway.model_ids().await,
        ["one/a", "one/b", "two/x", "two/y"]
    );
}

#[tokio::test]
async fn an_overlapping_refresh_keeps_newer_rows_and_switches() {
    // A slow pool's refresh must not publish its old copy of another pool
    // over a newer read, nor turn back on a model switched off meanwhile.
    let second = Arc::new(Mutex::new(vec!["x"]));
    let ids = second.clone();
    let slow = serve(Router::new().route(
        "/v1/models",
        get(|| async {
            tokio::time::sleep(Duration::from_millis(400)).await;
            Json(json!({"data":[{"id":"a"}]}))
        }),
    ))
    .await;
    let fast = serve(Router::new().route(
        "/v1/models",
        get(move || {
            let data: Vec<Value> = ids
                .lock()
                .unwrap()
                .iter()
                .map(|id| json!({ "id": id }))
                .collect();
            async move { Json(json!({ "data": data })) }
        }),
    ))
    .await;
    let gateway = Gateway::new(&[
        endpoint("one", &slow.url, "key"),
        endpoint("two", &fast.url, "key"),
    ])
    .await;
    assert_eq!(gateway.model_ids().await, ["one/a", "two/x"]);

    second.lock().unwrap().push("y");
    let state = gateway.state.clone();
    let slow_refresh = tokio::spawn(async move { state.refresh_models_in(Some("one")).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    gateway
        .admin(
            reqwest::Method::PATCH,
            "/models",
            Some(json!({"ids":["two/x"],"enabled":false})),
        )
        .await;
    gateway.state.refresh_models_in(Some("two")).await;
    slow_refresh.await.unwrap();
    assert_eq!(gateway.model_ids().await, ["one/a", "two/y"]);
}

#[tokio::test]
async fn a_failed_copilot_listing_is_marked_and_retried() {
    let server = serve(Router::new()).await;
    let gateway = Gateway::new(&[
        json!({"type":"copilot","access_token":"gho_fixture","base_url":server.url}),
    ])
    .await;
    assert_eq!(gateway.state.stale_pools(), ["copilot"]);
}

#[tokio::test]
async fn one_answering_account_does_not_hide_another_ones_failure() {
    // Plans differ, so one Copilot account's catalog cannot vouch for
    // another's: the pool stays stale and retried until both answer.
    let (answering, _) = copilot().await;
    let failing = serve(Router::new()).await;
    let gateway = Gateway::new(&[
        json!({"type":"copilot","access_token":"gho_one","base_url":answering.url}),
        json!({"type":"copilot","access_token":"gho_two","base_url":failing.url}),
    ])
    .await;
    assert_eq!(gateway.state.stale_pools(), ["copilot"]);
}

#[tokio::test]
async fn a_whole_provider_turns_off_in_one_call() {
    let server = serve(Router::new().route(
        "/v1/models",
        get(|| async { Json(json!({"data":[{"id":"a"},{"id":"b"},{"id":"c"}]})) }),
    ))
    .await;
    let gateway = Gateway::new(&[endpoint("one", &server.url, "key")]).await;
    gateway
        .admin(
            reqwest::Method::PATCH,
            "/models",
            Some(json!({"ids":["one/a","one/b"],"enabled":false})),
        )
        .await;
    assert_eq!(gateway.model_ids().await, ["one/c"]);
    let unknown = gateway
        .client
        .patch(format!("{}/admin/api/models", gateway.server.url))
        .json(&json!({"ids":["one/c","one/missing"],"enabled":false}))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), 404);
    assert_eq!(gateway.model_ids().await, ["one/c"]);
    gateway
        .admin(
            reqwest::Method::PATCH,
            "/models",
            Some(json!({"ids":["one/a","one/b"],"enabled":true})),
        )
        .await;
    assert_eq!(gateway.model_ids().await, ["one/a", "one/b", "one/c"]);
}

#[tokio::test]
async fn an_endpoint_without_a_listing_keeps_its_named_models() {
    // A request path that works beside a `/models` that never answers.
    let server = serve(Router::new()).await;
    let mut account = endpoint("mimo", &server.url, "key");
    account["models"] = json!(["mimo-v3"]);
    let gateway = Gateway::new(&[account]).await;
    assert_eq!(gateway.model_ids().await, ["mimo/mimo-v3"]);
    gateway.state.refresh_models().await;
    assert_eq!(gateway.model_ids().await, ["mimo/mimo-v3"]);
    assert!(gateway.state.stale_pools().is_empty());
}

#[tokio::test]
async fn a_narrowed_model_list_applies_while_the_listing_fails() {
    let failing = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let switch = failing.clone();
    let server = serve(Router::new().route(
        "/v1/models",
        get(move || {
            let failing = switch.load(std::sync::atomic::Ordering::Relaxed);
            async move {
                if failing {
                    StatusCode::BAD_GATEWAY.into_response()
                } else {
                    Json(json!({"data":[{"id":"a","context_length":64000},{"id":"b"}]}))
                        .into_response()
                }
            }
        }),
    ))
    .await;
    let gateway = Gateway::new(&[endpoint("openrouter", &server.url, "key")]).await;
    assert_eq!(gateway.model_ids().await, ["openrouter/a", "openrouter/b"]);
    failing.store(true, std::sync::atomic::Ordering::Relaxed);
    let id = gateway.state.accounts.list().await[0].id.clone();
    gateway
        .state
        .accounts
        .update_endpoint(
            &id,
            douane::subscriptions::EndpointPatch {
                models: Some(vec!["a".into()]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    gateway.state.refresh_models().await;
    assert_eq!(gateway.model_ids().await, ["openrouter/a"]);
    // The management catalog drops `b` too, and what the last listing said
    // about `a` survives the outage.
    let models = gateway.state.models.read().await;
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["openrouter/a"]);
    assert_eq!(models[0].context_length, Some(64000));
}

#[tokio::test]
async fn copilot_lists_enabled_models_and_uses_each_ones_path() {
    let (server, seen) = copilot().await;
    // OpenCode's own auth file is enough to bring the login over.
    let gateway = Gateway::new(&[json!({
        "github-copilot": {"type":"oauth","refresh":"gho_fixture","access":"gho_fixture","expires":0}
    })])
    .await;
    // The fixture's catalog lives on the loopback, as an enterprise host would.
    let accounts = gateway.admin(reqwest::Method::GET, "/accounts", None).await;
    assert_eq!(accounts["accounts"][0]["provider"], "copilot");
    let id = accounts["accounts"][0]["id"].as_str().unwrap().to_owned();
    gateway.state.accounts.delete(&id).await.unwrap();
    gateway
        .state
        .accounts
        .import(
            json!({"type":"copilot","access_token":"gho_fixture","base_url":server.url}),
            None,
        )
        .await
        .unwrap();
    gateway.state.refresh_models().await;
    assert_eq!(
        gateway.model_ids().await,
        [
            "copilot/claude-haiku-4.5",
            "copilot/gpt-4.1",
            "copilot/gpt-6-luna",
            "copilot/kimi-k3"
        ]
    );
    // A listed model the plan refuses says so, leaves the list and leaves
    // the account usable.
    let (status, body) = gateway
        .post("/v1/chat/completions", chat("copilot/kimi-k3"))
        .await;
    assert_eq!(status, 400, "{body}");
    assert!(
        body.to_string().contains("does not serve this model"),
        "{body}"
    );
    seen.seen.lock().unwrap().clear();
    gateway.state.refresh_models().await;
    assert!(
        !gateway
            .model_ids()
            .await
            .contains(&"copilot/kimi-k3".to_owned())
    );
    let accounts = gateway.admin(reqwest::Method::GET, "/accounts", None).await;
    assert_eq!(accounts["accounts"][0]["status"], "ready");

    // A Claude model keeps the Messages format; the user's turn is billed.
    let (status, body) = gateway
        .post(
            "/v1/messages",
            json!({"model":"copilot/claude-haiku-4.5","max_tokens":16,
                "messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    // A tool result is the agent's own follow-up.
    let (status, _) = gateway
        .post(
            "/v1/messages",
            json!({"model":"copilot/claude-haiku-4.5","max_tokens":16,"messages":[
                {"role":"user","content":"hi"},
                {"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"read","input":{}}]},
                {"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"done"}]}
            ]}),
        )
        .await;
    assert_eq!(status, 200);
    // A Responses-only model gets Responses; one without paths gets Chat.
    let (status, _) = gateway
        .post("/v1/chat/completions", chat("copilot/gpt-6-luna"))
        .await;
    assert_eq!(status, 200);
    let (status, _) = gateway
        .post(
            "/v1/messages",
            json!({"model":"copilot/gpt-4.1","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}),
        )
        .await;
    assert_eq!(status, 200);

    let calls = seen.seen.lock().unwrap().clone();
    let paths: Vec<&str> = calls.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/v1/messages",
            "/v1/messages",
            "/responses",
            "/chat/completions"
        ]
    );
    let initiators: Vec<&str> = calls
        .iter()
        .map(|c| c.headers["x-initiator"].to_str().unwrap())
        .collect();
    assert_eq!(initiators, ["user", "agent", "user", "user"]);
    for call in &calls {
        assert_eq!(call.headers["authorization"], "Bearer gho_fixture");
        assert_eq!(call.headers["openai-intent"], "conversation-edits");
        assert!(call.headers.contains_key("x-github-api-version"));
        assert!(!call.headers.contains_key("x-api-key"));
    }
    assert_eq!(calls[0].headers["anthropic-version"], "2023-06-01");
    assert_eq!(calls[0].body["model"], "claude-haiku-4.5");
}
