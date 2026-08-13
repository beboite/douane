//! End to end, over real sockets.
//!
//! A fake upstream stands in for Anthropic and records exactly what reached it.
//! That is the only way to prove the claims this project is built on: that the
//! virtual key never leaves, that the real one does, that a body carrying a
//! credential never gets sent at all. Asserting on the pieces separately would
//! leave the wiring between them untested, and the wiring is where a leak would
//! actually happen.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use douane::audit::Audit;
use douane::config::{Config, Provider, Route, ScrubConfig, Trust};
use douane::proxy::AppState;
use douane_vault::{Entry, EntryInfo, Secret, SecretBackend, SecretRef, VaultError};

// ------------------------------------------------------------ a fake vault --

/// Real vaults cost 64 MiB of Argon2 per open, which is right for a passphrase
/// and wrong for a test that opens one per case.
struct TestVault(BTreeMap<String, Entry>);

impl TestVault {
    fn new(pairs: &[(&str, &str)]) -> Self {
        Self(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), Entry::new(Secret::new(*v))))
                .collect(),
        )
    }
}

impl SecretBackend for TestVault {
    fn get(&self, r: &SecretRef) -> Option<&Secret> {
        self.0.get(&r.to_string()).map(|e| &e.value)
    }
    fn put(&mut self, r: &SecretRef, e: Entry) {
        self.0.insert(r.to_string(), e);
    }
    fn delete(&mut self, r: &SecretRef) -> bool {
        self.0.remove(&r.to_string()).is_some()
    }
    fn namespaces(&self) -> Vec<String> {
        let mut n: Vec<_> = self
            .0
            .keys()
            .filter_map(|k| SecretRef::parse(k).ok())
            .map(|r| r.namespace)
            .collect();
        n.dedup();
        n
    }
    /// Real, unlike a stub returning nothing, because what the dashboard's
    /// listing does and does not carry is one of the things worth proving here.
    fn list(&self) -> Vec<EntryInfo> {
        self.0
            .iter()
            .filter_map(|(k, e)| {
                let r = SecretRef::parse(k).ok()?;
                Some(EntryInfo {
                    placeholder: r.placeholder(),
                    namespace: r.namespace,
                    key: r.key,
                    fingerprint: e.value.fingerprint(),
                    bytes: e.value.len(),
                    updated_at: e.updated_at,
                    note: e.note.clone(),
                })
            })
            .collect()
    }
    fn persist(&self) -> Result<(), VaultError> {
        Ok(())
    }
    fn for_each_value(&self, f: &mut dyn FnMut(&SecretRef, &Secret)) {
        for (k, e) in &self.0 {
            f(&SecretRef::parse(k).unwrap(), &e.value);
        }
    }
}

// --------------------------------------------------------- a fake upstream --

#[derive(Debug, Clone)]
struct Seen {
    api_key: Option<String>,
    authorization: Option<String>,
    accept_encoding: Option<String>,
    body: serde_json::Value,
}

#[derive(Debug, Clone, Copy)]
enum Behaviour {
    /// A normal buffered answer carrying a `usage` object.
    Json,
    /// A two-event SSE stream, the way a real streamed answer arrives.
    Sse,
    Status(u16),
    /// An upstream that only sells the OpenAI shape, answering buffered.
    OpenaiJson,
    /// The same, streaming, ending with the usage-only frame that
    /// `stream_options.include_usage` buys.
    OpenaiSse,
}

struct Upstream {
    seen: Mutex<Vec<Seen>>,
    behaviour: Behaviour,
    /// What this upstream publishes as its catalogue, and how many times it was
    /// asked. Swappable so one test can watch a model appear and another watch
    /// one vanish.
    catalogue: Mutex<serde_json::Value>,
    catalogue_calls: Mutex<Vec<Option<String>>>,
    /// The public page saying which dialect each id answers in, absent until a
    /// test publishes one. A 404 here is the common case in the wild.
    dialects: Mutex<Option<serde_json::Value>>,
    dialect_calls: Mutex<Vec<Option<String>>>,
}

impl Upstream {
    async fn spawn(behaviour: Behaviour) -> (String, Arc<Self>) {
        let up = Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            behaviour,
            catalogue: Mutex::new(serde_json::json!({"data": []})),
            catalogue_calls: Mutex::new(Vec::new()),
            dialects: Mutex::new(None),
            dialect_calls: Mutex::new(Vec::new()),
        });
        let app = axum::Router::new()
            .route("/v1/messages", axum::routing::post(handle))
            // The other dialect's endpoint, so a test can prove douane asked for
            // the right path and not only that it sent the right body.
            .route("/chat/completions", axum::routing::post(handle))
            .route("/v1/models", axum::routing::get(handle_catalogue))
            .route("/models", axum::routing::get(handle_catalogue))
            .route("/api/pricing", axum::routing::get(handle_dialects))
            .with_state(up.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), up)
    }

    fn last(&self) -> Seen {
        self.seen
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("no call reached the upstream")
    }

    fn calls(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    /// Publish this catalogue from now on. Takes `(id, display_name, window)`.
    fn sells(&self, models: &[(&str, Option<&str>, Option<u64>)]) {
        let data: Vec<serde_json::Value> = models
            .iter()
            .map(|(id, label, ctx)| {
                let mut v = serde_json::json!({"type": "model", "id": id});
                if let Some(l) = label {
                    v["display_name"] = serde_json::json!(l);
                }
                if let Some(c) = ctx {
                    v["context_length"] = serde_json::json!(c);
                }
                v
            })
            .collect();
        *self.catalogue.lock().unwrap() = serde_json::json!({"data": data, "has_more": false});
    }

    /// Publish the dialect table. Takes `(id, endpoint types)`.
    fn speaks(&self, models: &[(&str, &[&str])]) {
        let data: Vec<serde_json::Value> = models
            .iter()
            .map(|(id, kinds)| {
                serde_json::json!({
                    "model_name": id,
                    "supported_endpoint_types": kinds,
                    // The page is also a price list, and douane must read none
                    // of this.
                    "model_ratio": 3.5,
                    "quota_type": 0,
                })
            })
            .collect();
        *self.dialects.lock().unwrap() = Some(serde_json::json!({"data": data}));
    }

    /// What each dialect request carried, if anything. It must carry nothing:
    /// the page is public and a request with no key cannot leak one.
    fn dialect_keys(&self) -> Vec<Option<String>> {
        self.dialect_calls.lock().unwrap().clone()
    }

    /// The credential each catalogue request arrived with. A listing scoped to
    /// a token is worthless if douane asks anonymously.
    fn catalogue_keys(&self) -> Vec<Option<String>> {
        self.catalogue_calls.lock().unwrap().clone()
    }
}

async fn handle_catalogue(State(up): State<Arc<Upstream>>, headers: HeaderMap) -> Response {
    up.catalogue_calls.lock().unwrap().push(
        headers
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
    );
    let body = up.catalogue.lock().unwrap().clone();
    axum::Json(body).into_response()
}

async fn handle_dialects(State(up): State<Arc<Upstream>>, headers: HeaderMap) -> Response {
    up.dialect_calls.lock().unwrap().push(
        headers
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
    );
    match up.dialects.lock().unwrap().clone() {
        Some(body) => axum::Json(body).into_response(),
        None => (axum::http::StatusCode::NOT_FOUND, "no such page").into_response(),
    }
}

async fn handle(
    State(up): State<Arc<Upstream>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let header = |n: &str| {
        headers
            .get(n)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    up.seen.lock().unwrap().push(Seen {
        api_key: header("x-api-key"),
        authorization: header("authorization"),
        accept_encoding: header("accept-encoding"),
        body: serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
    });

    match up.behaviour {
        Behaviour::Status(code) => (
            StatusCode::from_u16(code).unwrap(),
            axum::Json(serde_json::json!({"type":"error"})),
        )
            .into_response(),
        Behaviour::Json => axum::Json(serde_json::json!({
            "id": "msg_test",
            "type": "message",
            "content": [{"type":"text","text":"hello"}],
            "usage": {"input_tokens": 1000, "output_tokens": 250, "cache_read_input_tokens": 40}
        }))
        .into_response(),
        Behaviour::Sse => {
            let events = concat!(
                "event: message_start\n",
                "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1000,\"output_tokens\":1}}}\n\n",
                "event: message_delta\n",
                "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":250}}\n\n",
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
            );
            (
                [("content-type", "text/event-stream")],
                axum::body::Body::from(events),
            )
                .into_response()
        }
        Behaviour::OpenaiJson => axum::Json(serde_json::json!({
            "id": "chatcmpl-test",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": {"role": "assistant", "content": "hello"},
            }],
            // 1040 prompt tokens of which 40 were cached, which is 1000 fresh
            // once the shapes are lined up. The same numbers the Anthropic
            // upstream above reports, so the two paths can be compared.
            "usage": {
                "prompt_tokens": 1040,
                "completion_tokens": 250,
                "prompt_tokens_details": {"cached_tokens": 40},
            }
        }))
        .into_response(),
        Behaviour::OpenaiSse => {
            let events = concat!(
                "data: {\"id\":\"chatcmpl-test\",\"choices\":[{\"delta\":{\"content\":\"hel\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1040,\"completion_tokens\":250,\"prompt_tokens_details\":{\"cached_tokens\":40}}}\n\n",
                "data: [DONE]\n\n",
            );
            (
                [("content-type", "text/event-stream")],
                axum::body::Body::from(events),
            )
                .into_response()
        }
    }
}

// ------------------------------------------------------------ a real douane --

struct Broker {
    url: String,
    state: Arc<AppState>,
    _dir: tempfile::TempDir,
}

/// Stand up a broker with the given providers and routes, on a random port.
async fn broker(
    providers: Vec<(&str, &str, Trust)>,
    routes: Vec<(&str, &str, Option<&str>, Vec<&str>)>,
    vault_pairs: &[(&str, &str)],
    scrub: ScrubConfig,
    max_trust: Option<Trust>,
) -> Broker {
    broker_with(providers, routes, vault_pairs, scrub, max_trust, |_| {}).await
}

/// The same, with a last look at the config before it is frozen.
///
/// Reaching into a running `AppState` to change the table would mean adding a
/// setter that only a test wants, and the live table is deliberately only
/// rebuildable from the database.
async fn broker_with(
    providers: Vec<(&str, &str, Trust)>,
    routes: Vec<(&str, &str, Option<&str>, Vec<&str>)>,
    vault_pairs: &[(&str, &str)],
    scrub: ScrubConfig,
    max_trust: Option<Trust>,
    tweak: impl FnOnce(&mut Config),
) -> Broker {
    let mut config = Config {
        server: Default::default(),
        providers: BTreeMap::new(),
        routes: BTreeMap::new(),
        clients: BTreeMap::new(),
        scrub,
        egress: Default::default(),
    };
    config.server.admin_key = Some(SecretRef::parse("douane.admin").unwrap());

    for (name, base_url, trust) in providers {
        config.providers.insert(
            name.to_owned(),
            Provider {
                base_url: base_url.to_owned(),
                key: Some(SecretRef::parse(&format!("{name}.api_key")).unwrap()),
                auth_header: "x-api-key".to_owned(),
                bearer: false,
                wire: douane::wire::Wire::Anthropic,
                trust,
                timeout_secs: 10,
                prefix: None,
                models_path: None,
                catalog: true,
                dialects_path: None,
            },
        );
    }
    for (name, provider, model, fallback) in routes {
        config.routes.insert(
            name.to_owned(),
            Route {
                provider: provider.to_owned(),
                model: model.map(str::to_owned),
                fallback: fallback.into_iter().map(str::to_owned).collect(),
                label: None,
                context_length: None,
            },
        );
    }
    config.clients.insert(
        "test".to_owned(),
        douane::config::Client {
            key: SecretRef::parse("douane.vk_test").unwrap(),
            routes: Vec::new(),
            max_trust,
            note: None,
        },
    );
    tweak(&mut config);
    config.validate().unwrap();

    let vault = TestVault::new(vault_pairs);

    let dir = tempfile::tempdir().unwrap();
    let audit = Audit::open(dir.path().join("log.sqlite")).unwrap();

    let state = Arc::new(
        AppState::new(
            config,
            Box::new(vault),
            reqwest::Client::builder().no_proxy().build().unwrap(),
            audit,
        )
        .unwrap(),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = douane::router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    Broker {
        url: format!("http://{addr}"),
        state,
        _dir: dir,
    }
}

fn secrets() -> Vec<(&'static str, &'static str)> {
    vec![
        ("douane.vk_test", "vk-the-virtual-one"),
        // Two capped callers, for the tests that check a client is only told
        // about what it may actually reach.
        ("douane.vk_sensitive", "vk-sensitive-key"),
        ("douane.vk_narrow", "vk-narrow-key"),
        ("douane.admin", "admin-token-for-the-dashboard"),
        ("primary.api_key", "sk-real-primary-key-value"),
        ("backup.api_key", "sk-real-backup-key-value"),
        ("proxy.password", "Tr0ub4dor-and-3-more"),
    ]
}

fn ask(model: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "max_tokens": 64,
        "system": "You are a test agent.",
        "messages": [{"role": "user", "content": "hello"}]
    })
}

async fn call(b: &Broker, key: &str, body: serde_json::Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/v1/messages", b.url))
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .await
        .unwrap()
}

// ------------------------------------------------------------------ tests --

#[tokio::test]
async fn the_virtual_key_is_swapped_and_the_model_rewritten() {
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", Some("deepseek-chat"), vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    let res = call(&b, "vk-the-virtual-one", ask("haiku")).await;
    assert_eq!(res.status(), 200);

    let seen = up.last();
    // The real key arrived.
    assert_eq!(seen.api_key.as_deref(), Some("sk-real-primary-key-value"));
    // The virtual one did not, in either header form. This is the single most
    // important assertion in the whole suite: a regression here hands a
    // reseller a key that works against douane.
    assert_ne!(seen.api_key.as_deref(), Some("vk-the-virtual-one"));
    assert_eq!(seen.authorization, None);
    // The name the agent asked for was replaced by the one the table says.
    assert_eq!(seen.body["model"], "deepseek-chat");
    // Everything else travelled untouched.
    assert_eq!(seen.body["max_tokens"], 64);
    assert_eq!(seen.body["system"], "You are a test agent.");
}

#[tokio::test]
async fn an_unknown_virtual_key_never_reaches_upstream() {
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    let res = call(&b, "vk-i-made-this-up", ask("haiku")).await;
    assert_eq!(res.status(), 401);
    assert_eq!(up.calls(), 0);
}

#[tokio::test]
async fn a_dead_provider_falls_through_to_its_fallback() {
    let (dead_url, dead) = Upstream::spawn(Behaviour::Status(503)).await;
    let (live_url, live) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![
            ("primary", &dead_url, Trust::Low),
            ("backup", &live_url, Trust::Mid),
        ],
        vec![
            ("haiku", "primary", Some("cheap"), vec!["sonnet"]),
            ("sonnet", "backup", Some("expensive"), vec![]),
        ],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    let res = call(&b, "vk-the-virtual-one", ask("haiku")).await;
    assert_eq!(res.status(), 200);
    assert_eq!(dead.calls(), 1);
    assert_eq!(live.calls(), 1);
    // The fallback used its own model name, not the primary's.
    assert_eq!(live.last().body["model"], "expensive");
    assert_eq!(
        live.last().api_key.as_deref(),
        Some("sk-real-backup-key-value")
    );
}

#[tokio::test]
async fn a_bad_request_is_not_retried_elsewhere() {
    let (bad_url, bad) = Upstream::spawn(Behaviour::Status(400)).await;
    let (other_url, other) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![
            ("primary", &bad_url, Trust::Low),
            ("backup", &other_url, Trust::Mid),
        ],
        vec![
            ("haiku", "primary", None, vec!["sonnet"]),
            ("sonnet", "backup", None, vec![]),
        ],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    let res = call(&b, "vk-the-virtual-one", ask("haiku")).await;
    // The request itself is malformed. Every upstream would say the same, and
    // trying a second one only costs money and hides the real answer.
    assert_eq!(res.status(), 400);
    assert_eq!(bad.calls(), 1);
    assert_eq!(other.calls(), 0);
}

#[tokio::test]
async fn a_vault_value_never_leaves_for_a_reseller() {
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    let mut body = ask("haiku");
    body["messages"][0]["content"] =
        "here is my nas password, Tr0ub4dor-and-3-more, what do you think".into();

    let res = call(&b, "vk-the-virtual-one", body).await;
    assert_eq!(res.status(), 403);
    // Nothing was sent. Not a redacted version, not a truncated one: the
    // request never happened.
    assert_eq!(up.calls(), 0);

    let text = res.text().await.unwrap();
    // The refusal names the reference so the user can act on it, and does not
    // repeat the value back, which would put it in a log all over again.
    assert!(text.contains("proxy.password"), "{text}");
    assert!(!text.contains("Tr0ub4dor"), "{text}");

    let alerts = b.state.audit.alerts(10).unwrap();
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].action, "block");
    assert_eq!(alerts[0].label, "proxy.password");
}

#[tokio::test]
async fn a_vault_value_is_substituted_rather_than_sent_when_asked() {
    // The net instead of the wall: the request still goes out, but the
    // credential reads as its placeholder by the time the reseller sees it.
    // This covers a value pasted by the user exactly as much as one an agent
    // copied out of a file, because by this point they are the same bytes.
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    let scrub = ScrubConfig {
        vault_hit: douane::config::ByTrust {
            low: douane::config::Action::Substitute,
            mid: douane::config::Action::Substitute,
            max: douane::config::Action::Allow,
        },
        ..ScrubConfig::default()
    };
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        scrub,
        None,
    )
    .await;

    let mut body = ask("haiku");
    body["messages"][0]["content"] =
        "here is my nas password, Tr0ub4dor-and-3-more, what do you think".into();

    let res = call(&b, "vk-the-virtual-one", body).await;
    assert_eq!(res.status(), 200);
    assert_eq!(up.calls(), 1, "the request was forwarded, not refused");

    let seen = up.last();
    let content = seen.body["messages"][0]["content"].as_str().unwrap();
    assert!(
        !content.contains("Tr0ub4dor"),
        "the value must not reach the upstream: {content}"
    );
    assert!(
        content.contains("__DOUANE_PROXY_PASSWORD__"),
        "the placeholder travels in its place: {content}"
    );
    assert!(
        content.contains("here is my nas password"),
        "and the rest of the sentence is untouched: {content}"
    );

    // The audit says what was hidden, names the reference, and never stores
    // the value anywhere.
    let alerts = b.state.audit.alerts(10).unwrap();
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].action, "substitute");
    assert_eq!(alerts[0].label, "proxy.password");
    assert_eq!(alerts[0].kind, "vault");
    let json = serde_json::to_string(&alerts).unwrap();
    assert!(!json.contains("Tr0ub4dor"));
}

#[tokio::test]
async fn the_same_value_goes_through_to_a_local_model() {
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![("primary", &url, Trust::Max)],
        vec![("qwopus", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    let mut body = ask("qwopus");
    body["messages"][0]["content"] = "the password is Tr0ub4dor-and-3-more".into();

    let res = call(&b, "vk-the-virtual-one", body).await;
    assert_eq!(res.status(), 200);
    // Nothing leaves the LAN, so there is nothing to protect it from.
    assert_eq!(up.calls(), 1);
    assert!(b.state.audit.alerts(10).unwrap().is_empty());
}

#[tokio::test]
async fn a_client_capped_at_max_cannot_reach_a_reseller() {
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        Some(Trust::Max),
    )
    .await;

    let res = call(&b, "vk-the-virtual-one", ask("haiku")).await;
    assert_eq!(res.status(), 403);
    assert_eq!(up.calls(), 0);

    // The refusal has to be findable afterwards. Nothing was forwarded, so
    // there is no `calls` row and there never should be one: that table exists
    // to check a provider's invoice against what actually left. The alert log
    // is where douane records having decided something.
    let alerts = b.state.audit.alerts(10).unwrap();
    assert_eq!(alerts.len(), 1, "the refusal is recorded once");
    assert_eq!(alerts[0].kind, "trust");
    assert_eq!(alerts[0].action, "block");
    assert_eq!(alerts[0].label, "haiku", "the route that was refused");
    assert!(
        b.state.audit.recent(10, None, None).unwrap().is_empty(),
        "and no call row, because no call was made"
    );

    // And the client is told which cap stopped it, rather than being left to
    // guess against a config it cannot read.
    let body = res.text().await.unwrap();
    assert!(body.contains("cap of `max`"), "{body}");
    assert!(body.contains("trusted `low`"), "{body}");
}

#[tokio::test]
async fn a_route_a_client_is_not_listed_for_says_so_when_nothing_else_serves() {
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker_with(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
        |c| {
            // Allowed on a route that does not serve what was asked for, which
            // is the ordinary way a client ends up with an empty chain.
            c.clients.get_mut("test").unwrap().routes = vec!["sonnet".to_owned()];
            c.routes.insert(
                "sonnet".to_owned(),
                douane::config::Route {
                    provider: "primary".to_owned(),
                    model: None,
                    fallback: Vec::new(),
                    label: None,
                    context_length: None,
                },
            );
        },
    )
    .await;

    let res = call(&b, "vk-the-virtual-one", ask("haiku")).await;
    assert_eq!(res.status(), 403);
    assert_eq!(up.calls(), 0);

    let alerts = b.state.audit.alerts(10).unwrap();
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].kind, "route");
    assert_eq!(alerts[0].label, "haiku");

    let body = res.text().await.unwrap();
    assert!(body.contains("not allowed on this route"), "{body}");
}

#[tokio::test]
async fn a_skip_a_fallback_covers_is_not_worth_an_alert() {
    // The other half of the rule. A primary the client cannot use, followed by
    // a backup it can, is a routing decision and not a refusal: recording it
    // would fill the alert log with events nobody needs to act on, and bury the
    // ones they do.
    let (blocked_url, blocked) = Upstream::spawn(Behaviour::Json).await;
    let (ok_url, ok) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![
            ("primary", &blocked_url, Trust::Low),
            ("backup", &ok_url, Trust::Mid),
        ],
        vec![
            ("haiku", "primary", None, vec!["sonnet"]),
            ("sonnet", "backup", None, vec![]),
        ],
        &secrets(),
        ScrubConfig::default(),
        Some(Trust::Mid),
    )
    .await;

    let res = call(&b, "vk-the-virtual-one", ask("haiku")).await;
    assert_eq!(res.status(), 200);
    let _ = res.bytes().await.unwrap();

    assert_eq!(blocked.calls(), 0, "the reseller was above the cap");
    assert_eq!(ok.calls(), 1, "the fallback served it");
    assert!(
        b.state.audit.alerts(10).unwrap().is_empty(),
        "and nothing was refused, so nothing is reported"
    );
}

#[tokio::test]
async fn a_buffered_answer_is_counted() {
    let (url, _up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    let res = call(&b, "vk-the-virtual-one", ask("haiku")).await;
    assert_eq!(res.status(), 200);
    // Draining the body is what closes the ledger, exactly as a real client
    // reading the answer would.
    let _ = res.bytes().await.unwrap();

    let calls = wait_for_finished(&b).await;
    assert_eq!(calls[0].status, Some(200));
    assert_eq!(calls[0].input_tokens, Some(1000));
    assert_eq!(calls[0].output_tokens, Some(250));
    assert_eq!(calls[0].cache_read_tokens, Some(40));
    assert!(calls[0].bytes_down.unwrap() > 0);

    // And the counts reach the summary, where the trust signals are read.
    let s = b.state.audit.summary("1970-01-01T00:00:00Z", 24).unwrap();
    assert_eq!(s.totals.input_tokens, 1000);
    assert_eq!(s.totals.cache_read_tokens, 40);
}

#[tokio::test]
async fn an_upstream_that_only_speaks_openai_is_translated_both_ways() {
    // The reason this exists: crof.ai's Anthropic endpoint has been 502 since
    // 2026-08-01 while its OpenAI one answers fine. The unit tests in `wire`
    // prove the translation; this proves `forward` actually reaches for it, on
    // the right path, and that the ledger still fills in.
    let (url, up) = Upstream::spawn(Behaviour::OpenaiJson).await;
    let b = broker_with(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
        |c| {
            c.providers.get_mut("primary").unwrap().wire = douane::wire::Wire::Openai;
        },
    )
    .await;

    let mut body = ask("haiku");
    body["system"] = serde_json::json!([{"type": "text", "text": "be brief"}]);
    body["stop_sequences"] = serde_json::json!(["END"]);
    let res = call(&b, "vk-the-virtual-one", body).await;
    assert_eq!(res.status(), 200);
    let answer: serde_json::Value = res.json().await.unwrap();

    // What the upstream received is the other dialect.
    let sent = up.last().body;
    assert_eq!(sent["messages"][0]["role"], "system");
    assert_eq!(sent["messages"][0]["content"], "be brief");
    assert_eq!(sent["stop"][0], "END");
    assert!(
        sent.get("stop_sequences").is_none() && sent.get("system").is_none(),
        "the Anthropic-only fields must not travel: {sent}"
    );

    // What the client receives is its own, unchanged.
    assert_eq!(answer["type"], "message");
    assert_eq!(answer["role"], "assistant");
    assert_eq!(answer["content"][0]["text"], "hello");
    assert_eq!(answer["stop_reason"], "end_turn");

    // And the ledger reads the same numbers it would from an Anthropic upstream,
    // which is the whole point of translating before the tap rather than after.
    let calls = wait_for_finished(&b).await;
    assert_eq!(calls[0].input_tokens, Some(1000));
    assert_eq!(calls[0].output_tokens, Some(250));
    assert_eq!(calls[0].cache_read_tokens, Some(40));
}

#[tokio::test]
async fn a_translated_stream_is_counted_like_any_other() {
    let (url, up) = Upstream::spawn(Behaviour::OpenaiSse).await;
    let b = broker_with(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
        |c| {
            c.providers.get_mut("primary").unwrap().wire = douane::wire::Wire::Openai;
        },
    )
    .await;

    let mut body = ask("haiku");
    body["stream"] = serde_json::json!(true);
    let res = call(&b, "vk-the-virtual-one", body).await;
    assert_eq!(res.status(), 200);
    let text = res.text().await.unwrap();

    // Without this the upstream streams no usage at all and the row lands with
    // tokens unknown, which is the one thing douane exists to avoid.
    assert_eq!(
        up.last().body["stream_options"]["include_usage"],
        serde_json::json!(true),
        "a streamed request has to ask for its own counts"
    );

    // The client is handed Anthropic events, in the order one expects them.
    for event in [
        "message_start",
        "content_block_start",
        "content_block_delta",
        "content_block_stop",
        "message_delta",
        "message_stop",
    ] {
        assert!(text.contains(event), "missing {event} in:\n{text}");
    }

    let calls = wait_for_finished(&b).await;
    assert_eq!(calls[0].input_tokens, Some(1000));
    assert_eq!(calls[0].output_tokens, Some(250));
    assert_eq!(calls[0].cache_read_tokens, Some(40));
}

#[tokio::test]
async fn a_client_asking_for_gzip_does_not_blind_the_counter() {
    // Found in production on the first day. Every HTTP client asks for gzip;
    // the upstream obliges; douane then relays bytes it cannot read, and the
    // token counter records nothing at all, silently, on every single call.
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    let res = reqwest::Client::new()
        .post(format!("{}/v1/messages", b.url))
        .header("x-api-key", "vk-the-virtual-one")
        .header("accept-encoding", "gzip, deflate, br")
        .json(&ask("haiku"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let _ = res.bytes().await;

    assert_eq!(
        up.last().accept_encoding,
        None,
        "the header must not reach upstream, or the answer comes back unreadable"
    );

    let calls = wait_for_finished(&b).await;
    assert_eq!(calls[0].input_tokens, Some(1000));
    assert_eq!(calls[0].output_tokens, Some(250));
}

#[tokio::test]
async fn a_streamed_answer_is_counted_across_its_events() {
    let (url, _up) = Upstream::spawn(Behaviour::Sse).await;
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    let mut body = ask("haiku");
    body["stream"] = true.into();
    let res = call(&b, "vk-the-virtual-one", body).await;
    assert_eq!(res.status(), 200);
    let text = res.text().await.unwrap();
    // The stream arrived intact, event boundaries and all.
    assert!(text.contains("message_start"));
    assert!(text.contains("message_stop"));

    let calls = wait_for_finished(&b).await;
    assert_eq!(calls[0].input_tokens, Some(1000));
    // The last delta wins: the count is cumulative, so summing would double it.
    assert_eq!(calls[0].output_tokens, Some(250));
    assert!(calls[0].streamed);
}

#[tokio::test]
async fn the_dashboard_is_shut_without_its_token_and_repoints_a_route_with_it() {
    let (a_url, a_up) = Upstream::spawn(Behaviour::Json).await;
    let (b_url, b_up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![
            ("primary", &a_url, Trust::Low),
            ("backup", &b_url, Trust::Mid),
        ],
        vec![
            ("haiku", "primary", Some("cheap"), vec![]),
            ("sonnet", "backup", None, vec![]),
        ],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    let http = reqwest::Client::new();
    let admin = |path: &str| format!("{}/admin/api{path}", b.url);

    // No token, no answers.
    assert_eq!(
        http.get(admin("/config")).send().await.unwrap().status(),
        401
    );
    assert_eq!(
        http.get(admin("/config"))
            .header("x-douane-admin", "not-it")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );

    // The page itself is open: it is static markup and gives nothing away.
    let page = http.get(format!("{}/admin", b.url)).send().await.unwrap();
    assert_eq!(page.status(), 200);
    assert!(page.text().await.unwrap().contains("douane"));

    let with_token = |req: reqwest::RequestBuilder| {
        req.header("x-douane-admin", "admin-token-for-the-dashboard")
    };

    // Before: `haiku` lands on the reseller.
    call(&b, "vk-the-virtual-one", ask("haiku")).await;
    assert_eq!(a_up.calls(), 1);
    assert_eq!(b_up.calls(), 0);

    // Repoint it, with no restart.
    let res = with_token(http.put(admin("/routes/haiku")))
        .json(&serde_json::json!({"provider": "backup", "model": "somewhere-else"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    call(&b, "vk-the-virtual-one", ask("haiku")).await;
    assert_eq!(a_up.calls(), 1, "the old provider must not be called again");
    assert_eq!(b_up.calls(), 1);
    assert_eq!(b_up.last().body["model"], "somewhere-else");
    // And the real key that went with it is the new provider's, not the old.
    assert_eq!(
        b_up.last().api_key.as_deref(),
        Some("sk-real-backup-key-value")
    );

    // The config view marks the route as overridden and remembers the file's
    // version, so a human can see what reverting would do.
    let cfg: serde_json::Value = with_token(http.get(admin("/config")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let haiku = cfg["routes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "haiku")
        .unwrap();
    assert_eq!(haiku["overridden"], true);
    assert_eq!(haiku["base_provider"], "primary");

    // Revert, and it goes back to the file without one being written.
    let res = with_token(http.delete(admin("/routes/haiku")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    call(&b, "vk-the-virtual-one", ask("haiku")).await;
    assert_eq!(a_up.calls(), 2);
    assert_eq!(a_up.last().body["model"], "cheap");
}

/// The point of letting the dashboard write to the vault, and the thing that
/// would make it a bad idea if it did not hold.
///
/// A scrubber built once at startup cannot recognise a value deposited after
/// it, so the credential would travel to a reseller unwatched until the next
/// restart: the deposit would create exactly the leak the deposit was meant to
/// prevent. This proves the same body is let through before and refused after.
#[tokio::test]
async fn a_credential_deposited_through_the_dashboard_is_watched_on_the_way_out() {
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    let leaked = "hunter2-the-brand-new-one";
    let mut body = ask("haiku");
    body["messages"][0]["content"] = format!("the token is {leaked}").into();

    // Nothing knows this string yet, so it goes out like any other text.
    assert_eq!(
        call(&b, "vk-the-virtual-one", body.clone()).await.status(),
        200
    );
    assert_eq!(up.calls(), 1);

    let res = reqwest::Client::new()
        .put(format!("{}/admin/api/vault/scraper/token", b.url))
        .header("x-douane-admin", "admin-token-for-the-dashboard")
        .json(&serde_json::json!({"value": leaked, "note": "deposited by hand"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let stored: serde_json::Value = res.json().await.unwrap();
    // The answer describes the entry and does not repeat what was just sent.
    assert_eq!(stored["entry"]["placeholder"], "__DOUANE_SCRAPER_TOKEN__");
    assert!(!stored.to_string().contains(leaked));

    // Same body, same reseller, and now it does not leave at all.
    let res = call(&b, "vk-the-virtual-one", body).await;
    assert_eq!(res.status(), 403);
    assert_eq!(up.calls(), 1, "the deposited value must not have been sent");

    let text = res.text().await.unwrap();
    assert!(text.contains("scraper.token"), "{text}");
    assert!(!text.contains(leaked), "{text}");
}

#[tokio::test]
async fn no_vault_surface_hands_a_value_back() {
    let (url, _up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    let http = reqwest::Client::new();
    let admin = |path: &str| format!("{}/admin/api{path}", b.url);
    let with_token = |req: reqwest::RequestBuilder| {
        req.header("x-douane-admin", "admin-token-for-the-dashboard")
    };

    let listing: serde_json::Value = with_token(http.get(admin("/vault")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // Every value the vault holds, and none of them anywhere in the answer.
    let text = listing.to_string();
    for (_, value) in secrets() {
        assert!(!text.contains(value), "the listing carried {value}");
    }
    assert_eq!(
        listing["entries"].as_array().unwrap().len(),
        secrets().len()
    );

    // The references the running configuration authenticates with are marked,
    // because the delete handler refuses those and a button that cannot work is
    // worse than no button.
    let in_use: Vec<&str> = listing["in_use"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(in_use.contains(&"douane.vk_test"));
    assert!(in_use.contains(&"douane.admin"));
    assert!(in_use.contains(&"primary.api_key"));
    assert!(!in_use.contains(&"proxy.password"));

    // And one of them cannot be deleted from here, whatever the caller intends.
    let res = with_token(http.delete(admin("/vault/douane/vk_test")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 409);
    // Still authenticating, so nothing was half-deleted on the way to refusing.
    assert_eq!(
        call(&b, "vk-the-virtual-one", ask("haiku")).await.status(),
        200
    );
}

#[tokio::test]
async fn a_deposit_is_refused_when_it_could_never_be_read_back() {
    let (url, _up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    let http = reqwest::Client::new();
    let put = |path: &str, body: serde_json::Value| {
        http.put(format!("{}/admin/api/vault/{path}", b.url))
            .header("x-douane-admin", "admin-token-for-the-dashboard")
            .json(&body)
    };

    // A namespace with a dot in it resolves to a different reference than the
    // one that was typed, so the entry would be invisible to the config.
    assert_eq!(
        put("has.a.dot/key", serde_json::json!({"value": "x"}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    // An empty value is a deletion written as a deposit.
    assert_eq!(
        put("ns/key", serde_json::json!({"value": ""}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    // A note on nothing: there is no entry to annotate, and inventing an empty
    // one would put a reference in the listing that authenticates nothing.
    assert_eq!(
        put("ns/key", serde_json::json!({"note": "about what"}))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    // Nothing above stored anything.
    assert_eq!(b.state.vault_list().len(), secrets().len());
}

/// The token is read from the vault on every request rather than cached at
/// startup, so replacing it through the dashboard takes effect immediately. A
/// cached one would leave the old token working until a restart nobody would
/// think to do.
#[tokio::test]
async fn rotating_the_admin_token_takes_effect_at_once() {
    let (url, _up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    let http = reqwest::Client::new();
    let config = || format!("{}/admin/api/config", b.url);

    let res = http
        .put(format!("{}/admin/api/vault/douane/admin", b.url))
        .header("x-douane-admin", "admin-token-for-the-dashboard")
        .json(&serde_json::json!({"value": "the-new-admin-token"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    assert_eq!(
        http.get(config())
            .header("x-douane-admin", "admin-token-for-the-dashboard")
            .send()
            .await
            .unwrap()
            .status(),
        401,
        "the token that was just replaced must stop working"
    );
    assert_eq!(
        http.get(config())
            .header("x-douane-admin", "the-new-admin-token")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
}

#[tokio::test]
async fn an_unknown_provider_is_refused_rather_than_stored() {
    let (url, _up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    let res = reqwest::Client::new()
        .put(format!("{}/admin/api/routes/haiku", b.url))
        .header("x-douane-admin", "admin-token-for-the-dashboard")
        .json(&serde_json::json!({"provider": "a-provider-that-does-not-exist"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
    // A stored override naming nothing would silently take the route out of
    // service on the next restart.
    assert!(b.state.audit.overrides().unwrap().is_empty());
}

#[tokio::test]
async fn attribution_survives_into_the_summary() {
    let (url, _up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    // Two turns of one conversation, and one turn of another.
    for _ in 0..2 {
        let res = call(&b, "vk-the-virtual-one", ask("haiku")).await;
        let _ = res.bytes().await;
    }
    let mut other = ask("haiku");
    other["messages"][0]["content"] = "a different opening line".into();
    other["system"] = "You are a different agent.".into();
    let res = call(&b, "vk-the-virtual-one", other).await;
    let _ = res.bytes().await;

    wait_for_finished(&b).await;

    let s = b.state.audit.summary("1970-01-01T00:00:00Z", 24).unwrap();
    assert_eq!(s.totals.calls, 3);
    assert_eq!(s.totals.errors, 0);
    assert_eq!(s.totals.input_tokens, 3000);
    // Two distinct system prompts, so two agents.
    assert_eq!(s.by_agent.len(), 2);

    // Two distinct opening messages, so two conversations, and the first holds
    // both of its turns.
    let threads = b.state.audit.threads("1970-01-01T00:00:00Z", 10).unwrap();
    assert_eq!(threads.len(), 2);
    assert_eq!(threads.iter().map(|t| t.calls).sum::<i64>(), 3);
    assert!(threads.iter().any(|t| t.calls == 2));
}

/// The ledger closes when the response body is dropped, which happens on
/// another task, so a fresh read can catch the row still open.
async fn wait_for_finished(b: &Broker) -> Vec<douane::audit::Call> {
    for _ in 0..100 {
        let calls = b.state.audit.recent(100, None, None).unwrap();
        if !calls.is_empty() && calls.iter().all(|c| c.status.is_some()) {
            return calls;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("a call never closed its row");
}

// ------------------------------------------------------------------- MCP --

/// One stateless JSON-RPC exchange, the way an MCP client would make it.
async fn mcp(b: &Broker, key: &str, method: &str, params: serde_json::Value) -> serde_json::Value {
    reqwest::Client::new()
        .post(format!("{}/mcp", b.url))
        .header("x-api-key", key)
        .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn decide(b: &Broker, id: i64, body: serde_json::Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/admin/api/grants/{id}/decide", b.url))
        .header("x-douane-admin", "admin-token-for-the-dashboard")
        .json(&body)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn a_credential_request_waits_on_the_owner_and_hands_back_a_name() {
    let (url, _up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    // The agent asks, and is not blocked: it gets a task handle at once.
    let res = mcp(
        &b,
        "vk-the-virtual-one",
        "tools/call",
        serde_json::json!({"name": "credential_request", "arguments": {
            "reference": "proxy.password",
            "reason": "the scraping job needs the proxy login"
        }}),
    )
    .await;
    let task = &res["result"]["task"];
    assert_eq!(task["status"], "working");
    let task_id = task["taskId"].as_str().unwrap().to_owned();

    // Polling before the owner moves answers `working`, not an error and not
    // a block.
    let res = mcp(
        &b,
        "vk-the-virtual-one",
        "tasks/get",
        serde_json::json!({"taskId": task_id}),
    )
    .await;
    assert_eq!(res["result"]["task"]["status"], "working");

    // The owner approves from the dashboard.
    let pending = b.state.audit.grants(Some("pending"), 10).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].client, "test");
    assert_eq!(pending[0].reference, "proxy.password");
    let r = decide(
        &b,
        pending[0].id,
        serde_json::json!({"approve": true, "note": "ok for tonight"}),
    )
    .await;
    assert_eq!(r.status(), 200);

    // The next poll completes with the placeholder. The value is nowhere in
    // the answer, because this answer lands in a model's context.
    let res = mcp(
        &b,
        "vk-the-virtual-one",
        "tasks/get",
        serde_json::json!({"taskId": task_id}),
    )
    .await;
    let raw = res.to_string();
    assert_eq!(res["result"]["task"]["status"], "completed");
    assert_eq!(
        res["result"]["task"]["result"]["placeholder"],
        "__DOUANE_PROXY_PASSWORD__"
    );
    assert!(
        !raw.contains("Tr0ub4dor"),
        "a value crossed the MCP door: {raw}"
    );

    // Deciding twice does not work: the first word stands.
    let r = decide(&b, pending[0].id, serde_json::json!({"approve": false})).await;
    assert_eq!(r.status(), 409);
}

#[tokio::test]
async fn a_denied_request_fails_the_task_and_says_why() {
    let (url, _up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    let res = mcp(
        &b,
        "vk-the-virtual-one",
        "tools/call",
        serde_json::json!({"name": "credential_request", "arguments": {
            "reference": "proxy.password", "reason": "testing the refusal"
        }}),
    )
    .await;
    let task_id = res["result"]["task"]["taskId"].as_str().unwrap().to_owned();

    let pending = b.state.audit.grants(Some("pending"), 10).unwrap();
    decide(
        &b,
        pending[0].id,
        serde_json::json!({"approve": false, "note": "not for this job"}),
    )
    .await;

    let res = mcp(
        &b,
        "vk-the-virtual-one",
        "tasks/get",
        serde_json::json!({"taskId": task_id}),
    )
    .await;
    assert_eq!(res["result"]["task"]["status"], "failed");
    let msg = res["result"]["task"]["error"]["message"].as_str().unwrap();
    assert!(msg.contains("not for this job"), "{msg}");
}

#[tokio::test]
async fn the_mcp_door_is_shut_without_a_virtual_key() {
    let (url, _up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    let bare = reqwest::Client::new()
        .post(format!("{}/mcp", b.url))
        .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .send()
        .await
        .unwrap();
    assert_eq!(bare.status(), 401);

    let wrong = reqwest::Client::new()
        .post(format!("{}/mcp", b.url))
        .header("x-api-key", "vk-not-a-key")
        .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);
}

#[tokio::test]
async fn an_agents_task_is_invisible_to_another_agent() {
    let (url, _up) = Upstream::spawn(Behaviour::Json).await;
    let mut pairs = secrets();
    pairs.push(("douane.vk_other", "vk-the-other-one"));
    let b = broker_with(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &pairs,
        ScrubConfig::default(),
        None,
        |c| {
            c.clients.insert(
                "other".to_owned(),
                douane::config::Client {
                    key: SecretRef::parse("douane.vk_other").unwrap(),
                    routes: Vec::new(),
                    max_trust: None,
                    note: None,
                },
            );
        },
    )
    .await;

    let res = mcp(
        &b,
        "vk-the-virtual-one",
        "tools/call",
        serde_json::json!({"name": "credential_request", "arguments": {
            "reference": "proxy.password", "reason": "mine"
        }}),
    )
    .await;
    let task_id = res["result"]["task"]["taskId"].as_str().unwrap().to_owned();

    // The other agent polling the same handle learns nothing, and cannot
    // cancel it either.
    let res = mcp(
        &b,
        "vk-the-other-one",
        "tasks/get",
        serde_json::json!({"taskId": task_id}),
    )
    .await;
    assert!(res["error"]["message"].as_str().unwrap().contains("no task"));
    let res = mcp(
        &b,
        "vk-the-other-one",
        "tasks/cancel",
        serde_json::json!({"taskId": task_id}),
    )
    .await;
    assert!(res.get("error").is_some());

    // Its owner can withdraw it.
    let res = mcp(
        &b,
        "vk-the-virtual-one",
        "tasks/cancel",
        serde_json::json!({"taskId": task_id}),
    )
    .await;
    assert_eq!(res["result"]["status"], "cancelled");
}

#[tokio::test]
async fn a_deposit_over_mcp_is_watched_on_the_way_out() {
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    let value = "agent-made-credential-abcdef123456";
    let res = mcp(
        &b,
        "vk-the-virtual-one",
        "tools/call",
        serde_json::json!({"name": "vault_deposit", "arguments": {
            "reference": "vercel.deploy_token", "value": value
        }}),
    )
    .await;
    let text = res["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("__DOUANE_VERCEL_DEPLOY_TOKEN__"), "{text}");
    assert!(!text.contains(value), "the deposit answer repeated the value");

    // The scrubber knows it at once: the same value heading for the reseller
    // in a chat body is refused.
    let mut body = ask("haiku");
    body["messages"][0]["content"] = format!("the token is {value}").into();
    let res = call(&b, "vk-the-virtual-one", body).await;
    assert_eq!(res.status(), 403);
    assert_eq!(up.calls(), 0);

    // Write-only also means write-once from an agent: no overwriting.
    let res = mcp(
        &b,
        "vk-the-virtual-one",
        "tools/call",
        serde_json::json!({"name": "vault_deposit", "arguments": {
            "reference": "vercel.deploy_token", "value": "something-else-entirely"
        }}),
    )
    .await;
    assert_eq!(res["result"]["isError"], true);
}

#[tokio::test]
async fn douanes_own_keys_are_not_grantable() {
    let (url, _up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    let res = mcp(
        &b,
        "vk-the-virtual-one",
        "tools/call",
        serde_json::json!({"name": "credential_request", "arguments": {
            "reference": "primary.api_key", "reason": "let me talk to the provider myself"
        }}),
    )
    .await;
    assert_eq!(res["result"]["isError"], true);
    assert!(b.state.audit.grants(None, 10).unwrap().is_empty());
}

#[tokio::test]
async fn a_filed_request_rings_the_owners_bell() {
    // A tiny listener standing in for ntfy.
    let heard: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = heard.clone();
    let app = axum::Router::new().route(
        "/ring",
        axum::routing::post(move |body: String| {
            let sink = sink.clone();
            async move {
                sink.lock().unwrap().push(body);
                axum::http::StatusCode::OK
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let notify_url = format!("http://{}/ring", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let (url, _up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker_with(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
        |c| c.server.notify_url = Some(notify_url),
    )
    .await;

    mcp(
        &b,
        "vk-the-virtual-one",
        "tools/call",
        serde_json::json!({"name": "credential_request", "arguments": {
            "reference": "proxy.password", "reason": "need the proxy"
        }}),
    )
    .await;

    let messages = heard.lock().unwrap().clone();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].contains("test"), "{}", messages[0]);
    assert!(messages[0].contains("proxy.password"), "{}", messages[0]);
    assert!(!messages[0].contains("Tr0ub4dor"), "{}", messages[0]);
}

#[tokio::test]
async fn redemption_hands_the_value_to_the_approved_client_and_nobody_else() {
    // The one surface that answers with a value, and everything that gates it.
    let (url, _up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;
    let http = reqwest::Client::new();
    let redeem = |key: &'static str, reference: &'static str| {
        http.post(format!("{}/agent/api/use", b.url))
            .header("x-api-key", key)
            .json(&serde_json::json!({ "reference": reference }))
            .send()
    };

    // No key, no door.
    let bare = http
        .post(format!("{}/agent/api/use", b.url))
        .json(&serde_json::json!({ "reference": "proxy.password" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bare.status(), 401);

    // No request filed: nothing to redeem.
    let res = redeem("vk-the-virtual-one", "proxy.password").await.unwrap();
    assert_eq!(res.status(), 404);

    // Filed but pending: still no.
    mcp(
        &b,
        "vk-the-virtual-one",
        "tools/call",
        serde_json::json!({"name": "credential_request", "arguments": {
            "reference": "proxy.password", "reason": "the job needs the proxy"
        }}),
    )
    .await;
    let res = redeem("vk-the-virtual-one", "proxy.password").await.unwrap();
    assert_eq!(res.status(), 403);
    let text = res.text().await.unwrap();
    assert!(text.contains("waiting"), "{text}");
    assert!(!text.contains("Tr0ub4dor"), "{text}");

    // Approved: the value comes back, to this client, on this path only.
    let id = b.state.audit.grants(Some("pending"), 10).unwrap()[0].id;
    decide(&b, id, serde_json::json!({"approve": true, "ttl_hours": 2})).await;
    let res = redeem("vk-the-virtual-one", "proxy.password").await.unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["value"], "Tr0ub4dor-and-3-more");
    assert!(body["expires_at"].as_str().is_some());

    // The grant remembers it was used.
    let g = b.state.audit.grant_by_id(id).unwrap().unwrap();
    assert!(g.used_at.is_some());

    // A denial reads as the owner's word, not as a broken server.
    mcp(
        &b,
        "vk-the-virtual-one",
        "tools/call",
        serde_json::json!({"name": "credential_request", "arguments": {
            "reference": "backup.api_key", "reason": "reaching the backup myself"
        }}),
    )
    .await;
    let id = b.state.audit.grants(Some("pending"), 10).unwrap()[0].id;
    decide(
        &b,
        id,
        serde_json::json!({"approve": false, "note": "use the relay"}),
    )
    .await;
    let res = redeem("vk-the-virtual-one", "backup.api_key").await.unwrap();
    assert_eq!(res.status(), 403);
    let text = res.text().await.unwrap();
    assert!(text.contains("use the relay"), "{text}");
    assert!(!text.contains("sk-real-backup-key-value"), "{text}");
}

#[tokio::test]
async fn no_mcp_surface_hands_a_value_back() {
    let (url, _up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    for method in ["tools/list", "tasks/get"] {
        let res = mcp(
            &b,
            "vk-the-virtual-one",
            method,
            serde_json::json!({"taskId": "task_none"}),
        )
        .await;
        let text = res.to_string();
        for (_, value) in secrets() {
            assert!(!text.contains(value), "{method} carried {value}");
        }
    }

    let res = mcp(
        &b,
        "vk-the-virtual-one",
        "tools/call",
        serde_json::json!({"name": "vault_list", "arguments": {}}),
    )
    .await;
    let text = res.to_string();
    for (_, value) in secrets() {
        assert!(!text.contains(value), "vault_list carried {value}");
    }
    assert!(text.contains("proxy"), "the names are the point: {text}");
}

#[tokio::test]
async fn the_key_check_tells_a_dead_key_from_a_live_one() {
    // Two providers, two keys, one of which the upstream no longer accepts.
    // The dashboard's question is exactly this: which of my keys still work.
    let (live_url, live_up) = Upstream::spawn(Behaviour::Json).await;
    let (dead_url, _dead_up) = Upstream::spawn(Behaviour::Status(401)).await;
    let b = broker(
        vec![
            ("primary", &live_url, Trust::Low),
            ("backup", &dead_url, Trust::Low),
        ],
        vec![
            ("haiku", "primary", Some("deepseek-chat"), vec![]),
            ("opus", "backup", None, vec![]),
        ],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    // The door is shut without the admin token: a probe spends money.
    let bare = reqwest::Client::new()
        .post(format!("{}/admin/api/providers/probe", b.url))
        .send()
        .await
        .unwrap();
    assert_eq!(bare.status(), 401);

    let res = reqwest::Client::new()
        .post(format!("{}/admin/api/providers/probe", b.url))
        .header("x-douane-admin", "admin-token-for-the-dashboard")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let answer: serde_json::Value = res.json().await.unwrap();
    let probes = answer["probes"].as_array().unwrap();
    assert_eq!(probes.len(), 2);

    let by_name = |n: &str| {
        probes
            .iter()
            .find(|p| p["provider"] == n)
            .unwrap_or_else(|| panic!("no probe for {n}"))
    };
    let live = by_name("primary");
    assert_eq!(live["ok"], true);
    assert_eq!(live["status"], 200);
    assert_eq!(live["model"], "deepseek-chat", "the route's model, as a real call would send");

    let dead = by_name("backup");
    assert_eq!(dead["ok"], false);
    assert_eq!(dead["status"], 401);

    // The probe went out with the provider's real credential and a handful of
    // tokens, exactly like traffic and nothing like a listing.
    let seen = live_up.last();
    assert_eq!(seen.api_key.as_deref(), Some("sk-real-primary-key-value"));
    assert_eq!(seen.body["max_tokens"], 16);
    assert_eq!(seen.body["model"], "deepseek-chat");
}

#[tokio::test]
async fn the_key_check_speaks_each_providers_own_dialect() {
    // A provider douane translates for must be probed in the shape it sells,
    // or every probe of it would read as a dead key.
    let (url, up) = Upstream::spawn(Behaviour::OpenaiJson).await;
    let b = broker_with(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
        |c| {
            c.providers.get_mut("primary").unwrap().wire = douane::wire::Wire::Openai;
        },
    )
    .await;

    let res = reqwest::Client::new()
        .post(format!("{}/admin/api/providers/probe", b.url))
        .header("x-douane-admin", "admin-token-for-the-dashboard")
        .send()
        .await
        .unwrap();
    let answer: serde_json::Value = res.json().await.unwrap();
    assert_eq!(answer["probes"][0]["ok"], true);

    // It landed on the OpenAI path, in the OpenAI shape.
    let seen = up.last();
    assert_eq!(seen.body["max_tokens"], 16);
    assert!(seen.body.get("messages").is_some());
}

#[tokio::test]
async fn a_failing_probe_reports_the_upstreams_words_but_never_a_value() {
    // An upstream whose error page echoes the credential back is exactly the
    // page the dashboard must not display verbatim.
    let (url, _up) = Upstream::spawn(Behaviour::Status(401)).await;
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    let res = reqwest::Client::new()
        .post(format!("{}/admin/api/providers/probe", b.url))
        .header("x-douane-admin", "admin-token-for-the-dashboard")
        .send()
        .await
        .unwrap();
    let answer: serde_json::Value = res.json().await.unwrap();
    let text = answer.to_string();
    for (_, value) in secrets() {
        assert!(!text.contains(value), "the probe answer carried {value}");
    }
    assert_eq!(answer["probes"][0]["status"], 401);
}

// -------------------------------------------------------------- catalogue --
//
// The claim these prove: douane can learn what an upstream sells without any of
// it becoming reachable. Discovery fills a table, the owner switches a row on,
// and only then does a name become a route. Each test below is one of the ways
// that could go wrong.

/// Admin helper. The catalogue screen is a handful of small calls, so a test
/// reads better with one.
async fn admin(
    b: &Broker,
    method: reqwest::Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> (reqwest::StatusCode, serde_json::Value) {
    let mut req = reqwest::Client::new()
        .request(method, format!("{}{path}", b.url))
        .header("x-douane-admin", "admin-token-for-the-dashboard");
    if let Some(v) = body {
        req = req.json(&v);
    }
    let res = req.send().await.unwrap();
    let status = res.status();
    let json = res.json().await.unwrap_or(serde_json::Value::Null);
    (status, json)
}

async fn refresh(b: &Broker) -> serde_json::Value {
    admin(b, reqwest::Method::POST, "/admin/api/models/refresh", None)
        .await
        .1
}

async fn catalogue(b: &Broker) -> serde_json::Value {
    admin(b, reqwest::Method::GET, "/admin/api/models", None)
        .await
        .1
}

async fn enable(b: &Broker, route: &str, on: bool) -> reqwest::StatusCode {
    admin(
        b,
        reqwest::Method::PUT,
        &format!("/admin/api/models/{route}/enabled"),
        Some(serde_json::json!({ "enabled": on })),
    )
    .await
    .0
}

/// What one client is told it may ask for.
async fn listing(b: &Broker, key: &str) -> (reqwest::StatusCode, Vec<String>) {
    let res = reqwest::Client::new()
        .get(format!("{}/v1/models", b.url))
        .header("x-api-key", key)
        .send()
        .await
        .unwrap();
    let status = res.status();
    let v: serde_json::Value = res.json().await.unwrap_or(serde_json::Value::Null);
    let ids = v["data"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| m["id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    (status, ids)
}

#[tokio::test]
async fn a_discovered_model_is_not_a_route_until_it_is_switched_on() {
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    up.sells(&[(
        "deepseek-v4-flash",
        Some("DeepSeek v4 Flash"),
        Some(1_000_000),
    )]);
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    let out = refresh(&b).await;
    assert_eq!(out["refreshed"][0]["ok"], true);
    assert_eq!(out["refreshed"][0]["found"], 1);
    assert_eq!(out["refreshed"][0]["added"], 1);

    // Discovery is not permission. The model sits in the catalogue switched
    // off, and asking for it is still a 404 that never reaches the upstream.
    let cat = catalogue(&b).await;
    assert_eq!(cat["models"][0]["route"], "primary-deepseek-v4-flash");
    assert_eq!(cat["models"][0]["enabled"], false);
    assert_eq!(cat["models"][0]["label"], "DeepSeek v4 Flash");
    assert_eq!(cat["models"][0]["context_length"], 1_000_000);
    let res = call(&b, "vk-the-virtual-one", ask("primary-deepseek-v4-flash")).await;
    assert_eq!(res.status(), 404);
    assert_eq!(up.calls(), 0);

    // The owner's click is what creates the route.
    assert_eq!(enable(&b, "primary-deepseek-v4-flash", true).await, 200);
    let res = call(&b, "vk-the-virtual-one", ask("primary-deepseek-v4-flash")).await;
    assert_eq!(res.status(), 200);
    // It left under the id the upstream spells, with the real key.
    assert_eq!(up.last().body["model"], "deepseek-v4-flash");
    assert_eq!(
        up.last().api_key.as_deref(),
        Some("sk-real-primary-key-value")
    );

    // And switching it back off retires the route while the row survives.
    assert_eq!(enable(&b, "primary-deepseek-v4-flash", false).await, 200);
    let res = call(&b, "vk-the-virtual-one", ask("primary-deepseek-v4-flash")).await;
    assert_eq!(res.status(), 404);
}

#[tokio::test]
async fn the_catalogue_is_asked_with_the_providers_own_credential() {
    // A listing is usually scoped to the token, so asking anonymously answers
    // about somebody else's account.
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    up.sells(&[("a-model", None, None)]);
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    refresh(&b).await;
    let keys = up.catalogue_keys();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].as_deref(), Some("sk-real-primary-key-value"));
}

#[tokio::test]
async fn the_file_wins_a_collision_and_the_dashboard_says_so() {
    // An upstream that starts publishing an id matching a hand-written route
    // must not be able to repoint it, however that row is switched.
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    let (other, other_up) = Upstream::spawn(Behaviour::Json).await;
    up.sells(&[("haiku", None, None)]);
    let b = broker_with(
        vec![("primary", &url, Trust::Low), ("backup", &other, Trust::Low)],
        vec![("haiku", "backup", Some("the-one-from-the-file"), vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
        |c| {
            // An empty prefix makes the generated name land exactly on the
            // route the file owns, which is the collision being tested.
            c.providers.get_mut("primary").unwrap().prefix = Some(String::new());
        },
    )
    .await;

    refresh(&b).await;
    assert_eq!(enable(&b, "haiku", true).await, 200);

    // The call still goes where the file said, and the switch that claims
    // otherwise is reported rather than silently ignored.
    let res = call(&b, "vk-the-virtual-one", ask("haiku")).await;
    assert_eq!(res.status(), 200);
    assert_eq!(other_up.last().body["model"], "the-one-from-the-file");
    assert_eq!(up.calls(), 0);
    let cat = catalogue(&b).await;
    assert_eq!(cat["collisions"]["haiku"]["kind"], "conflict");
    assert!(
        cat["collisions"]["haiku"]["detail"]
            .as_str()
            .unwrap()
            .contains("douane.toml")
    );
}

#[tokio::test]
async fn the_file_serving_the_same_model_is_not_a_conflict() {
    // The dashboard used to shout about this: a hand-written route and a
    // discovered row naming the same model on the same provider is the file
    // and the upstream agreeing, and the model answers. Reporting it the same
    // way as a real clash is how a screen teaches people to ignore it.
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    up.sells(&[("kimi-k3", None, None)]);
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("primary-kimi-k3", "primary", Some("kimi-k3"), vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    refresh(&b).await;
    assert_eq!(enable(&b, "primary-kimi-k3", true).await, 200);

    let cat = catalogue(&b).await;
    assert_eq!(cat["collisions"]["primary-kimi-k3"]["kind"], "already_in_file");
    assert_eq!(cat["collisions"]["primary-kimi-k3"]["other"], "primary-kimi-k3");
    // And it is not a broken route: the call goes through.
    let res = call(&b, "vk-the-virtual-one", ask("primary-kimi-k3")).await;
    assert_eq!(res.status(), 200);
    assert_eq!(up.last().body["model"], "kimi-k3");
}

#[tokio::test]
async fn the_same_model_under_a_second_name_is_refused_once() {
    // A curated route and a discovered row can point at one model on one
    // provider. Installing both puts it in every menu twice with nothing to
    // choose between the entries, so the file keeps its name and the row says
    // where the model went.
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    up.sells(&[("glm-5.2", None, None), ("kimi-k3", None, None)]);
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("glm", "primary", Some("glm-5.2"), vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;
    refresh(&b).await;
    enable(&b, "primary-glm-5.2", true).await;
    enable(&b, "primary-kimi-k3", true).await;

    let cat = catalogue(&b).await;
    assert_eq!(cat["collisions"]["primary-glm-5.2"]["kind"], "already_in_file");
    assert_eq!(cat["collisions"]["primary-glm-5.2"]["other"], "glm");
    let (_, ids) = listing(&b, "vk-the-virtual-one").await;
    assert!(ids.contains(&"glm".to_owned()));
    assert!(!ids.contains(&"primary-glm-5.2".to_owned()));
    // The one the file says nothing about is served as usual.
    assert!(ids.contains(&"primary-kimi-k3".to_owned()));
}

#[tokio::test]
async fn a_model_sold_on_another_surface_is_not_served_here() {
    // One account, two surfaces, one catalogue listing every id on both. The
    // ids that answer in the other dialect must not become routes here: they
    // would look served and fail at call time.
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    up.sells(&[("claude-opus-5", None, None), ("gpt-5.6-terra", None, None)]);
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    // Before the site publishes anything, both ids look servable and both are
    // switched on.
    let out = refresh(&b).await;
    assert_eq!(out["refreshed"][0]["found"], 2);
    assert_eq!(out["refreshed"][0]["speaks"], 2);
    assert_eq!(enable(&b, "primary-claude-opus-5", true).await, 200);
    assert_eq!(enable(&b, "primary-gpt-5.6-terra", true).await, 200);

    up.speaks(&[
        ("claude-opus-5", &["anthropic", "openai"]),
        ("gpt-5.6-terra", &["openai", "openai-response"]),
    ]);
    let out = refresh(&b).await;
    assert_eq!(out["refreshed"][0]["speaks"], 1);
    // The page is public, and douane asks for it with nothing attached.
    assert!(up.dialect_keys().iter().all(Option::is_none));
    // A row the owner switched on is never deleted under them, however wrong
    // the surface turns out to be. It is refused instead, and says so.
    assert_eq!(out["refreshed"][0]["dropped"], 0);
    let cat = catalogue(&b).await;
    assert_eq!(
        cat["collisions"]["primary-gpt-5.6-terra"]["kind"],
        "wrong_dialect"
    );
    assert!(cat["collisions"]["primary-claude-opus-5"].is_null());

    let before = up.calls();
    let res = call(&b, "vk-the-virtual-one", ask("primary-gpt-5.6-terra")).await;
    assert_eq!(res.status(), 404);
    assert_eq!(up.calls(), before, "the refused model reached the upstream");
    let res = call(&b, "vk-the-virtual-one", ask("primary-claude-opus-5")).await;
    assert_eq!(res.status(), 200);

    // Switched off, it is not kept as a row nobody may ever switch on again:
    // it leaves the table at the next sweep.
    assert_eq!(enable(&b, "primary-gpt-5.6-terra", false).await, 200);
    let out = refresh(&b).await;
    assert_eq!(out["refreshed"][0]["dropped"], 1);
    let cat = catalogue(&b).await;
    let rows = cat["models"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["route"], "primary-claude-opus-5");
}

#[tokio::test]
async fn serving_everything_a_provider_sells_skips_what_it_cannot_reach() {
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    up.sells(&[
        ("claude-opus-5", None, None),
        ("gpt-5.6-terra", None, None),
        ("sora-video", None, None),
    ]);
    up.speaks(&[
        ("claude-opus-5", &["anthropic", "openai"]),
        ("gpt-5.6-terra", &["openai"]),
        ("sora-video", &["Unified video format"]),
    ]);
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;
    // The sweep keeps only what this surface can reach, so the video and the
    // OpenAI-only id never make it to the screen in the first place.
    let out = refresh(&b).await;
    assert_eq!(out["refreshed"][0]["found"], 3);
    assert_eq!(out["refreshed"][0]["dropped"], 2);
    assert_eq!(catalogue(&b).await["models"].as_array().unwrap().len(), 1);

    let (status, body) = admin(
        &b,
        reqwest::Method::PUT,
        "/admin/api/providers/primary/models/enabled",
        Some(serde_json::json!({ "enabled": true })),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["changed"], 1);
    assert_eq!(body["skipped"], 0);

    let (_, ids) = listing(&b, "vk-the-virtual-one").await;
    assert!(ids.contains(&"primary-claude-opus-5".to_owned()));
    assert!(!ids.contains(&"primary-gpt-5.6-terra".to_owned()));
}

#[tokio::test]
async fn a_hand_written_route_carries_its_own_name_and_window() {
    // Otherwise a launcher keeps a table of exactly these two fields and drifts
    // from the file. The file also outranks the catalogue on both: a curated
    // route is named by its author, not by the reseller underneath.
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    up.sells(&[("claude-opus-5", Some("Opus 5 (reseller)"), Some(200_000))]);
    let b = broker_with(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", Some("claude-opus-5"), vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
        |c| {
            let r = c.routes.get_mut("haiku").unwrap();
            r.label = Some("Opus 5 (1M)".to_owned());
            r.context_length = Some(1_000_000);
        },
    )
    .await;
    refresh(&b).await;

    let res = reqwest::Client::new()
        .get(format!("{}/v1/models", b.url))
        .header("x-api-key", "vk-the-virtual-one")
        .send()
        .await
        .unwrap();
    let v: serde_json::Value = res.json().await.unwrap();
    let row = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "haiku")
        .cloned()
        .unwrap();
    assert_eq!(row["display_name"], "Opus 5 (1M)");
    assert_eq!(row["context_length"], 1_000_000);
}

#[tokio::test]
async fn a_client_can_ask_for_one_upstreams_names_and_no_more() {
    // A launcher's menu has one entry per subscription, so it has to know which
    // names belong to which. The filter can only ever narrow: a client capped
    // to one provider learns nothing about the other by naming it.
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    let (other, _) = Upstream::spawn(Behaviour::Json).await;
    up.sells(&[("kimi-k3", None, None)]);
    let b = broker_with(
        vec![("primary", &url, Trust::Low), ("backup", &other, Trust::Low)],
        vec![
            ("haiku", "primary", None, vec![]),
            ("spare", "backup", None, vec![]),
        ],
        &secrets(),
        ScrubConfig::default(),
        None,
        |c| {
            c.clients.insert(
                "narrow".to_owned(),
                douane::config::Client {
                    key: SecretRef::parse("douane.vk_narrow").unwrap(),
                    routes: vec!["spare".to_owned()],
                    max_trust: None,
                    note: None,
                },
            );
        },
    )
    .await;
    refresh(&b).await;
    enable(&b, "primary-kimi-k3", true).await;

    let listing_for = |key: &'static str, q: &'static str| {
        let url = b.url.clone();
        async move {
            let res = reqwest::Client::new()
                .get(format!("{url}/v1/models{q}"))
                .header("x-api-key", key)
                .send()
                .await
                .unwrap();
            let v: serde_json::Value = res.json().await.unwrap();
            v["data"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|m| m["id"].as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        }
    };

    let all = listing_for("vk-the-virtual-one", "").await;
    assert!(all.contains(&"haiku".to_owned()) && all.contains(&"spare".to_owned()));
    let one = listing_for("vk-the-virtual-one", "?provider=primary").await;
    assert_eq!(one, vec!["haiku".to_owned(), "primary-kimi-k3".to_owned()]);
    let two = listing_for("vk-the-virtual-one", "?provider=primary,backup").await;
    assert_eq!(two.len(), 3);
    // The cap still wins: naming the provider a client may not reach answers
    // nothing rather than answering more.
    assert!(
        listing_for("vk-narrow-key", "?provider=primary")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn a_discovered_row_can_be_corrected_and_the_correction_survives_a_sweep() {
    // The window and the display name are exactly what a launcher used to keep
    // its own table for, and these listings publish neither.
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    up.sells(&[("kimi-k3", None, None)]);
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;
    refresh(&b).await;

    let (status, _) = admin(
        &b,
        reqwest::Method::PATCH,
        "/admin/api/models/primary-kimi-k3",
        Some(serde_json::json!({
            "label": "Kimi K3",
            "context_length": 256000,
            "route": "k3"
        })),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(enable(&b, "k3", true).await, 200);

    refresh(&b).await;
    let cat = catalogue(&b).await;
    let row = cat["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["route"] == "k3")
        .cloned()
        .unwrap();
    assert_eq!(row["label"], "Kimi K3");
    assert_eq!(row["context_length"], 256_000);
    assert_eq!(row["enabled"], true);

    // And the client is told both, so it stops carrying its own copy.
    let res = reqwest::Client::new()
        .get(format!("{}/v1/models", b.url))
        .header("x-api-key", "vk-the-virtual-one")
        .send()
        .await
        .unwrap();
    let v: serde_json::Value = res.json().await.unwrap();
    let listed = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "k3")
        .cloned()
        .unwrap();
    assert_eq!(listed["display_name"], "Kimi K3");
    assert_eq!(listed["context_length"], 256_000);
}

#[tokio::test]
async fn a_dialect_page_that_is_not_there_refuses_nothing() {
    // Most sites publish none. Silence must never read as a refusal, or the
    // catalogue would empty itself on every provider that has no such page.
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    up.sells(&[("gpt-5.6-terra", None, None)]);
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    let out = refresh(&b).await;
    assert_eq!(out["refreshed"][0]["ok"], true);
    assert_eq!(out["refreshed"][0]["speaks"], 1);
    assert_eq!(enable(&b, "primary-gpt-5.6-terra", true).await, 200);
    let res = call(&b, "vk-the-virtual-one", ask("primary-gpt-5.6-terra")).await;
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn a_refresh_never_switches_a_model_on_or_off() {
    // The owner's decision survives every sweep in both directions. A refresh
    // that enabled something would be a broker widening its own surface; one
    // that disabled something would cut a working route on a listing.
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    up.sells(&[("stays", None, None), ("goes", None, None)]);
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    refresh(&b).await;
    enable(&b, "primary-stays", true).await;
    enable(&b, "primary-goes", true).await;

    // The upstream drops one and grows another.
    up.sells(&[("stays", None, None), ("new", None, None)]);
    let out = refresh(&b).await;
    assert_eq!(out["refreshed"][0]["added"], 1);
    assert_eq!(out["refreshed"][0]["missing"], 1);

    let cat = catalogue(&b).await;
    let row = |r: &str| {
        cat["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["route"] == r)
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    };
    // Still enabled, no longer listed, and still serving: an upstream that
    // stops listing a model it still answers on is common, and cutting the
    // route on that basis would be douane breaking itself.
    assert_eq!(row("primary-goes")["enabled"], true);
    assert_eq!(row("primary-goes")["present"], false);
    assert_eq!(row("primary-stays")["present"], true);
    // The new one arrived switched off, like every discovery.
    assert_eq!(row("primary-new")["enabled"], false);

    let res = call(&b, "vk-the-virtual-one", ask("primary-goes")).await;
    assert_eq!(res.status(), 200);
    let res = call(&b, "vk-the-virtual-one", ask("primary-new")).await;
    assert_eq!(res.status(), 404);
}

#[tokio::test]
async fn a_model_can_be_added_by_hand_for_an_upstream_with_no_catalogue() {
    // The case the manual path exists for: a site publishing nothing, or
    // publishing a listing narrower than what the token can actually reach.
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker_with(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
        |c| {
            c.providers.get_mut("primary").unwrap().catalog = false;
        },
    )
    .await;

    // A refresh says so plainly rather than reporting an empty catalogue.
    let out = refresh(&b).await;
    assert_eq!(out["refreshed"][0]["ok"], false);
    assert!(
        out["refreshed"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("by hand")
    );

    let (status, _) = admin(
        &b,
        reqwest::Method::PUT,
        "/admin/api/models",
        Some(serde_json::json!({
            "provider": "primary",
            "upstream_id": "grok-4.6",
            "label": "Grok 4.6",
            "context_length": 500000,
            "enabled": true,
        })),
    )
    .await;
    assert_eq!(status, 200);

    let res = call(&b, "vk-the-virtual-one", ask("primary-grok-4.6")).await;
    assert_eq!(res.status(), 200);
    assert_eq!(up.last().body["model"], "grok-4.6");

    let cat = catalogue(&b).await;
    assert_eq!(cat["models"][0]["source"], "manual");
    assert_eq!(cat["models"][0]["context_length"], 500_000);
}

#[tokio::test]
async fn a_hand_added_model_cannot_take_a_name_the_file_owns() {
    let (url, _up) = Upstream::spawn(Behaviour::Json).await;
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;

    let (status, body) = admin(
        &b,
        reqwest::Method::PUT,
        "/admin/api/models",
        Some(serde_json::json!({
            "provider": "primary",
            "upstream_id": "whatever",
            "route": "haiku",
            "enabled": true,
        })),
    )
    .await;
    assert_eq!(status, 409);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("douane.toml")
    );

    // A name that would not survive a URL or a command line is refused before
    // it reaches the database.
    let (status, _) = admin(
        &b,
        reqwest::Method::PUT,
        "/admin/api/models",
        Some(serde_json::json!({
            "provider": "primary",
            "upstream_id": "whatever",
            "route": "one that/../escapes",
        })),
    )
    .await;
    assert_eq!(status, 400);

    // And so is a provider that is not in the file.
    let (status, _) = admin(
        &b,
        reqwest::Method::PUT,
        "/admin/api/models",
        Some(serde_json::json!({
            "provider": "ghost",
            "upstream_id": "whatever",
        })),
    )
    .await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn the_model_listing_shows_a_client_only_what_it_may_reach() {
    // The listing is a client's whole view of the broker, so it answers per
    // client and not per instance: a capped client learning that a reseller
    // route exists would be a map of every upstream, handed out on any key.
    let (cheap, cheap_up) = Upstream::spawn(Behaviour::Json).await;
    let (house, _house_up) = Upstream::spawn(Behaviour::Json).await;
    cheap_up.sells(&[("flash", Some("Flash"), Some(128_000))]);
    let b = broker_with(
        vec![
            ("primary", &cheap, Trust::Low),
            ("backup", &house, Trust::Max),
        ],
        vec![
            ("haiku", "primary", None, vec![]),
            ("qwopus", "backup", None, vec![]),
        ],
        &secrets(),
        ScrubConfig::default(),
        None,
        |c| {
            c.clients.insert(
                "sensitive".to_owned(),
                douane::config::Client {
                    key: SecretRef::parse("douane.vk_sensitive").unwrap(),
                    routes: Vec::new(),
                    max_trust: Some(Trust::Max),
                    note: None,
                },
            );
            c.clients.insert(
                "narrow".to_owned(),
                douane::config::Client {
                    key: SecretRef::parse("douane.vk_narrow").unwrap(),
                    routes: vec!["qwopus".to_owned()],
                    max_trust: None,
                    note: None,
                },
            );
        },
    )
    .await;

    refresh(&b).await;
    enable(&b, "primary-flash", true).await;

    let (status, all) = listing(&b, "vk-the-virtual-one").await;
    assert_eq!(status, 200);
    assert!(all.contains(&"haiku".to_owned()));
    assert!(all.contains(&"qwopus".to_owned()));
    assert!(all.contains(&"primary-flash".to_owned()));

    // Capped at `max`: the reseller routes are not merely refused on use, they
    // are not listed at all.
    let (_, capped) = listing(&b, "vk-sensitive-key").await;
    assert_eq!(capped, vec!["qwopus".to_owned()]);

    // Restricted to one route by name: same answer, different reason.
    let (_, narrow) = listing(&b, "vk-narrow-key").await;
    assert_eq!(narrow, vec!["qwopus".to_owned()]);

    // An unknown key gets a refusal, not an empty list.
    let (status, _) = listing(&b, "vk-not-a-key").await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn the_listing_carries_what_a_launcher_needs_and_nothing_about_upstreams() {
    let (url, up) = Upstream::spawn(Behaviour::Json).await;
    up.sells(&[("flash", Some("Flash"), Some(128_000))]);
    let b = broker(
        vec![("primary", &url, Trust::Low)],
        vec![("haiku", "primary", None, vec![])],
        &secrets(),
        ScrubConfig::default(),
        None,
    )
    .await;
    refresh(&b).await;
    enable(&b, "primary-flash", true).await;

    let res = reqwest::Client::new()
        .get(format!("{}/v1/models", b.url))
        .header("x-api-key", "vk-the-virtual-one")
        .send()
        .await
        .unwrap();
    let v: serde_json::Value = res.json().await.unwrap();
    let flash = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "primary-flash")
        .cloned()
        .unwrap();
    assert_eq!(flash["display_name"], "Flash");
    assert_eq!(flash["context_length"], 128_000);
    // A route written in the file has no catalogue row, so it answers as
    // itself rather than inventing a label.
    let plain = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "haiku")
        .cloned()
        .unwrap();
    assert_eq!(plain["display_name"], "haiku");

    // Nothing in the answer names an upstream, a base URL or a credential.
    let text = v.to_string();
    assert!(!text.contains("http://"), "the listing leaked a base URL");
    assert!(!text.contains("127.0.0.1"), "the listing leaked a host");
    for (_, value) in secrets() {
        assert!(!text.contains(value), "the listing carried {value}");
    }
}
