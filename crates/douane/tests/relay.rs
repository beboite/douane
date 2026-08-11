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
}

struct Upstream {
    seen: Mutex<Vec<Seen>>,
    behaviour: Behaviour,
}

impl Upstream {
    async fn spawn(behaviour: Behaviour) -> (String, Arc<Self>) {
        let up = Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            behaviour,
        });
        let app = axum::Router::new()
            .route("/v1/messages", axum::routing::post(handle))
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
                trust,
                timeout_secs: 10,
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
