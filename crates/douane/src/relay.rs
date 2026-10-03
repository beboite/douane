use crate::{
    ledger::{Call, Ledger},
    state::AppState,
    subscriptions::{AccountCredential, SubscriptionProvider},
    translate::{self, Bridge, Dialect, Frames},
};
use axum::{
    Json,
    body::{Body, Bytes},
    extract::{OriginalUri, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Instant,
};

pub fn error(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(json!({"error":{"type":"proxy_error","message":message}})),
    )
        .into_response()
}
pub fn endpoint(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/v1") && path.starts_with("/v1/") {
        format!("{base}{}", &path[3..])
    } else {
        format!("{base}{path}")
    }
}

pub(crate) fn upstream(
    account: &AccountCredential,
    client: Dialect,
    model: &str,
) -> (String, Dialect) {
    use SubscriptionProvider::*;
    let (base, wire, path) = match account.provider {
        Codex => (
            "https://chatgpt.com/backend-api/codex",
            Dialect::Responses,
            "/responses",
        ),
        Claude => (
            "https://api.anthropic.com",
            Dialect::Messages,
            "/v1/messages",
        ),
        Xai => (
            "https://cli-chat-proxy.grok.com/v1",
            Dialect::Chat,
            "/v1/chat/completions",
        ),
        OpencodeGo => {
            let wire = if client == Dialect::Responses && model.starts_with("muse-") {
                Dialect::Responses
            } else {
                Dialect::Chat
            };
            (
                "https://opencode.ai/zen/go",
                wire,
                if wire == Dialect::Responses {
                    "/v1/responses"
                } else {
                    "/v1/chat/completions"
                },
            )
        }
        Kimi => (
            "https://api.kimi.com/coding",
            Dialect::Messages,
            "/v1/messages",
        ),
        Qwen => (
            "https://portal.qwen.ai",
            Dialect::Chat,
            "/v1/chat/completions",
        ),
        OpenaiCompatible => {
            let wire = client;
            (
                "https://api.openai.com",
                wire,
                match wire {
                    Dialect::Responses => "/v1/responses",
                    Dialect::Messages => "/v1/messages",
                    _ => "/v1/chat/completions",
                },
            )
        }
        Gemini => (
            "https://cloudcode-pa.googleapis.com",
            Dialect::Gemini,
            "/v1internal:generateContent",
        ),
        Antigravity => (
            "https://daily-cloudcode-pa.googleapis.com",
            Dialect::Gemini,
            "/v1internal:generateContent",
        ),
    };
    (
        endpoint(account.base_url.as_deref().unwrap_or(base), path),
        wire,
    )
}

pub(crate) fn upstream_headers(account: &AccountCredential, incoming: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in incoming {
        if !matches!(
            name.as_str(),
            "host"
                | "authorization"
                | "x-api-key"
                | "api-key"
                | "x-douane-admin"
                | "x-management-key"
                | "x-douane-provider"
                | "proxy-authorization"
                | "connection"
                | "keep-alive"
                | "te"
                | "trailer"
                | "upgrade"
                | "content-length"
                | "transfer-encoding"
                | "accept-encoding"
                | "cookie"
                | "origin"
                | "referer"
        ) && !name.as_str().starts_with("sec-websocket-")
        {
            out.insert(name.clone(), value.clone());
        }
    }
    out.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    if let Ok(token) = format!("Bearer {}", account.access_token).parse() {
        out.insert(header::AUTHORIZATION, token);
    }
    match account.provider {
        SubscriptionProvider::Claude | SubscriptionProvider::Kimi => {
            out.entry("anthropic-version")
                .or_insert(header::HeaderValue::from_static("2023-06-01"));
            if account.provider == SubscriptionProvider::Claude {
                let original = out
                    .get("anthropic-beta")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                let beta = if original.split(',').any(|s| s.trim() == "oauth-2025-04-20") {
                    original.to_owned()
                } else {
                    format!("oauth-2025-04-20,claude-code-20250219,{original}")
                        .trim_end_matches(',')
                        .to_owned()
                };
                if let Ok(value) = beta.parse() {
                    out.insert("anthropic-beta", value);
                }
                out.entry("user-agent")
                    .or_insert(header::HeaderValue::from_static(
                        "claude-cli/2.1.280 (external, cli)",
                    ));
                out.entry("x-app")
                    .or_insert(header::HeaderValue::from_static("cli"));
            }
        }
        SubscriptionProvider::Codex => {
            if let Some(id) = &account.account_id {
                if let Ok(value) = id.parse() {
                    out.insert("chatgpt-account-id", value);
                }
            }
            out.entry("originator")
                .or_insert(header::HeaderValue::from_static("codex_cli_rs"));
            out.entry("user-agent")
                .or_insert(header::HeaderValue::from_static("codex_cli_rs/0.155.0"));
        }
        SubscriptionProvider::Xai => {
            out.insert(
                "x-xai-token-auth",
                header::HeaderValue::from_static("xai-grok-cli"),
            );
            out.insert(
                "x-grok-client-version",
                header::HeaderValue::from_static("1.0.44"),
            );
            out.insert(
                "user-agent",
                header::HeaderValue::from_static(
                    "grok-pager/1.0.44 grok-shell/1.0.44 (linux; x86_64)",
                ),
            );
            if let Some(id) = &account.account_id {
                if let Ok(v) = id.parse() {
                    out.insert("x-userid", v);
                }
            }
        }
        SubscriptionProvider::OpencodeGo => {
            out.entry("user-agent")
                .or_insert(header::HeaderValue::from_static(concat!(
                    "douane/",
                    env!("CARGO_PKG_VERSION")
                )));
            if !out.contains_key("x-opencode-session") {
                if let Some(value) = ["session_id", "session-id", "thread-id"]
                    .iter()
                    .find_map(|h| incoming.get(*h))
                {
                    out.insert("x-opencode-session", value.clone());
                }
            }
        }
        SubscriptionProvider::Antigravity => {
            out.insert(
                header::USER_AGENT,
                header::HeaderValue::from_static("antigravity/hub/2.9.1"),
            );
            out.insert(
                "x-goog-api-client",
                header::HeaderValue::from_static("gl-node/22.21.1"),
            );
        }
        _ => {}
    }
    out
}

pub(crate) async fn choose(
    state: &AppState,
    body: &Value,
    headers: &HeaderMap,
) -> Result<(String, Vec<AccountCredential>, Option<String>), String> {
    let model = body["model"]
        .as_str()
        .filter(|m| !m.is_empty())
        .ok_or("model is required")?;
    let session = crate::affinity::session_key(headers, body);
    let (upstream, accounts) = state
        .candidates(
            model,
            headers
                .get("x-douane-provider")
                .and_then(|v| v.to_str().ok()),
            session.as_deref(),
        )
        .await?;
    Ok((upstream, accounts, session))
}

pub async fn forward(
    State(state): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let from = if uri.path().ends_with("/messages") {
        Dialect::Messages
    } else if uri.path().ends_with("/chat/completions") {
        Dialect::Chat
    } else {
        Dialect::Responses
    };
    let mut value: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return error(StatusCode::BAD_REQUEST, "body must be valid JSON"),
    };
    let stream = value["stream"].as_bool().unwrap_or(false);
    let (model, candidates, session) = match choose(&state, &value, &headers).await {
        Ok(v) => v,
        Err(e) => return error(StatusCode::SERVICE_UNAVAILABLE, &e),
    };
    value["model"] = json!(model);
    let mut last_status = StatusCode::BAD_GATEWAY;
    for candidate in candidates {
        let mut account = match state.oauth.credential(&candidate.id).await {
            Ok(a) => a,
            Err(_) => {
                let _ = state
                    .accounts
                    .cooldown(&candidate.id, 300, "authentication needs reconnecting")
                    .await;
                continue;
            }
        };
        let (mut url, to) = upstream(&account, from, &model);
        if uri.path().ends_with("/responses/compact") {
            if to != Dialect::Responses {
                return error(
                    StatusCode::BAD_REQUEST,
                    "this provider does not offer native compaction",
                );
            }
            url.push_str("/compact");
        }
        let mut translated = match translate::request(&value, from, to) {
            Ok(v) => v,
            Err(e) => return error(StatusCode::BAD_REQUEST, &e),
        };
        let codex_sse =
            account.provider == SubscriptionProvider::Codex && !uri.path().ends_with("/compact");
        if codex_sse {
            translated["stream"] = json!(true);
            translated["store"] = json!(false);
            if translated.get("instructions").is_none() {
                translated["instructions"] = json!("");
            }
            for key in ["max_output_tokens", "max_tokens", "temperature", "top_p"] {
                translated.as_object_mut().map(|m| m.remove(key));
            }
            if translated.get("prompt_cache_key").is_none() {
                if let Some(key) = &session {
                    translated["prompt_cache_key"] = json!(key);
                }
            }
        }
        if to == Dialect::Gemini {
            if stream {
                url = url.replace(":generateContent", ":streamGenerateContent?alt=sse");
            }
            translated = json!({"model":model,"project":account.project_id,"request":translated});
            if account.provider == SubscriptionProvider::Antigravity {
                translated["userAgent"] = json!("antigravity");
                translated["requestType"] = json!("agent");
                translated["requestId"] = json!(format!("douane-{:016x}", rand::random::<u64>()));
            }
        }
        let bytes = match serde_json::to_vec(&translated) {
            Ok(v) => Bytes::from(v),
            Err(_) => return error(StatusCode::BAD_REQUEST, "cannot serialize request"),
        };
        let mut call = CallGuard::new(state.ledger.clone(), &account, &model);
        let mut retry_auth = true;
        let response = loop {
            let timeout = state.config.read().await.settings.request_timeout_seconds;
            let result = state
                .client
                .post(&url)
                .timeout(std::time::Duration::from_secs(timeout))
                .headers(upstream_headers(&account, &headers))
                .body(bytes.clone())
                .send()
                .await;
            match result {
                Ok(r) if r.status() == StatusCode::UNAUTHORIZED && retry_auth => {
                    retry_auth = false;
                    match state.oauth.force_refresh(&account.id).await {
                        Ok(fresh) => account = fresh,
                        Err(_) => break Ok(r),
                    }
                }
                result => break result,
            }
        };
        let response = match response {
            Ok(r) => r,
            Err(_) => {
                call.fail("upstream connection failed");
                let _ = state
                    .accounts
                    .cooldown(&account.id, 10, "upstream connection failed")
                    .await;
                continue;
            }
        };
        last_status = response.status();
        call.call.status = last_status.as_u16();
        if !last_status.is_success() {
            call.fail(match last_status {
                StatusCode::TOO_MANY_REQUESTS => "upstream quota or rate limit reached",
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                    "upstream authentication refused"
                }
                _ => "upstream request refused",
            });
            if last_status == StatusCode::TOO_MANY_REQUESTS || last_status.is_server_error() {
                let wait = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(30);
                let _ = state
                    .accounts
                    .cooldown(&account.id, wait, "upstream temporarily unavailable")
                    .await;
                continue;
            }
            if matches!(
                last_status,
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            ) {
                let _ = state
                    .accounts
                    .cooldown(&account.id, 300, "authentication needs reconnecting")
                    .await;
                continue;
            }
            let _ = state
                .accounts
                .mark_error(&account.id, Some("upstream request refused".into()))
                .await;
            return error(
                last_status,
                call.call
                    .error
                    .as_deref()
                    .unwrap_or("upstream request refused"),
            );
        }
        if let Some(key) = &session {
            state
                .affinity
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .bind(account.provider.as_str(), key, &account.id);
        }
        let is_sse = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("text/event-stream"));
        if stream && is_sse {
            return stream_response(response, from, to, model, call);
        }
        let result = if is_sse {
            collect_response(response, to, &mut call).await
        } else {
            match read_bounded(response, 32 * 1024 * 1024).await {
                Ok(b) => serde_json::from_slice::<Value>(&b)
                    .map_err(|_| "upstream returned invalid JSON".into()),
                Err(e) => Err(e),
            }
        };
        let result = match result {
            Ok(v) => v,
            Err(e) => {
                call.fail(&e);
                return error(StatusCode::BAD_GATEWAY, &e);
            }
        };
        call.observe(&result);
        let result = match translate::response(&result, to, from, &model) {
            Ok(v) => v,
            Err(e) => {
                call.fail(&e);
                return error(StatusCode::BAD_GATEWAY, &e);
            }
        };
        call.complete = true;
        return Json(result).into_response();
    }
    error(
        last_status,
        "all accounts for this provider are unavailable; check their quotas and connection status",
    )
}

pub(crate) async fn read_bounded(
    response: reqwest::Response,
    cap: usize,
) -> Result<Vec<u8>, String> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "upstream body interrupted")?;
        if bytes.len() + chunk.len() > cap {
            return Err("upstream response exceeds size limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn collect_response(
    response: reqwest::Response,
    to: Dialect,
    call: &mut CallGuard,
) -> Result<Value, String> {
    let mut decoder = Frames::default();
    let mut upstream = response.bytes_stream();
    let mut result = None;
    while let Some(chunk) = upstream.next().await {
        for frame in decoder.feed(&chunk.map_err(|_| "upstream stream interrupted")?)? {
            call.observe(&frame);
            if frame["type"] == "response.failed" || frame["type"] == "error" {
                return Err("upstream reported a stream error".into());
            }
            if to == Dialect::Responses
                && matches!(
                    frame["type"].as_str(),
                    Some("response.completed" | "response.incomplete")
                )
            {
                result = Some(frame["response"].clone());
            }
        }
    }
    for frame in decoder.finish()? {
        call.observe(&frame);
        if frame["type"] == "response.failed" || frame["type"] == "error" {
            return Err("upstream reported a stream error".into());
        }
        if to == Dialect::Responses
            && matches!(
                frame["type"].as_str(),
                Some("response.completed" | "response.incomplete")
            )
        {
            result = Some(frame["response"].clone());
        }
    }
    result.ok_or("upstream stream closed before completing its response".into())
}

fn stream_response(
    response: reqwest::Response,
    from: Dialect,
    to: Dialect,
    model: String,
    call: CallGuard,
) -> Response {
    let bridge = if from == to {
        None
    } else {
        Some(Bridge::new(to, from, &model))
    };
    let state = (
        Box::pin(response.bytes_stream()),
        Frames::default(),
        bridge,
        call,
        false,
    );
    let stream = futures_util::stream::unfold(
        state,
        |(mut upstream, mut frames, mut bridge, mut call, mut done)| async move {
            if done {
                return None;
            }
            loop {
                let next = upstream.next().await;
                let output: Result<Option<Bytes>, String> = match next {
                    Some(Ok(chunk)) => match frames.feed(&chunk) {
                        Ok(events) => {
                            let mut translated = String::new();
                            let mut failure = None;
                            for event in events {
                                call.observe(&event);
                                if let Some(b) = bridge.as_mut() {
                                    match b.feed(event) {
                                        Ok(s) => translated.push_str(&s),
                                        Err(e) => {
                                            failure = Some(e);
                                            break;
                                        }
                                    }
                                }
                            }
                            if let Some(e) = failure {
                                Err(e)
                            } else if bridge.is_some() {
                                Ok((!translated.is_empty()).then(|| Bytes::from(translated)))
                            } else {
                                Ok(Some(chunk))
                            }
                        }
                        Err(e) => Err(e),
                    },
                    Some(Err(_)) => Err("upstream stream interrupted".into()),
                    None => {
                        done = true;
                        if let Some(b) = bridge.as_mut() {
                            let tail = (|| -> Result<String, String> {
                                let mut output = String::new();
                                for event in frames.finish()? {
                                    call.observe(&event);
                                    output.push_str(&b.feed(event)?);
                                }
                                output.push_str(&b.finish()?);
                                Ok(output)
                            })();
                            match tail {
                                Ok(t) => {
                                    call.complete = true;
                                    Ok((!t.is_empty()).then(|| Bytes::from(t)))
                                }
                                Err(e) => Err(e),
                            }
                        } else {
                            match frames.finish() {
                                Ok(events) => {
                                    for event in events {
                                        call.observe(&event);
                                    }
                                    if call.complete {
                                        Ok(None)
                                    } else {
                                        Err("upstream stream closed before completion".into())
                                    }
                                }
                                Err(e) => Err(e),
                            }
                        }
                    }
                };
                match output {
                    Ok(Some(bytes)) => {
                        return Some((
                            Ok::<Bytes, std::io::Error>(bytes),
                            (upstream, frames, bridge, call, done),
                        ));
                    }
                    Ok(None) if done => return None,
                    Ok(None) => continue,
                    Err(e) => {
                        call.fail(&e);
                        done = true;
                        let body =
                            json!({"type":"error","error":{"type":"upstream_error","message":e}});
                        return Some((
                            Ok(Bytes::from(format!("event: error\ndata: {body}\n\n"))),
                            (upstream, frames, bridge, call, done),
                        ));
                    }
                }
            }
        },
    );
    (
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
            (header::HeaderName::from_static("x-accel-buffering"), "no"),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

pub(crate) struct CallGuard {
    pub call: Call,
    pub complete: bool,
    start: Instant,
    ledger: Arc<Ledger>,
    uncached_input: Option<u64>,
}
impl CallGuard {
    pub fn new(ledger: Arc<Ledger>, account: &AccountCredential, model: &str) -> Self {
        ledger.in_flight.fetch_add(1, Ordering::Relaxed);
        Self {
            call: Call {
                id: format!("{:032x}", rand::random::<u128>()),
                started_at: crate::subscriptions::now(),
                provider: account.provider.as_str().into(),
                account_id: account.id.clone(),
                model: model.into(),
                status: 200,
                ..Call::default()
            },
            complete: false,
            start: Instant::now(),
            ledger,
            uncached_input: None,
        }
    }
    pub fn fail(&mut self, error: &str) {
        self.call.error = Some(error.to_owned());
        self.complete = true;
    }
    pub fn observe(&mut self, value: &Value) {
        let usage = value
            .get("usage")
            .or_else(|| value.pointer("/message/usage"))
            .or_else(|| value.pointer("/response/usage"))
            .or_else(|| value.get("usageMetadata"))
            .or_else(|| value.pointer("/response/usageMetadata"));
        if let Some(u) = usage {
            self.call.input_tokens = u["input_tokens"]
                .as_u64()
                .or_else(|| u["prompt_tokens"].as_u64())
                .or_else(|| u["promptTokenCount"].as_u64())
                .unwrap_or(self.call.input_tokens);
            self.call.output_tokens = u["output_tokens"]
                .as_u64()
                .or_else(|| u["completion_tokens"].as_u64())
                .or_else(|| u["candidatesTokenCount"].as_u64())
                .unwrap_or(self.call.output_tokens);
            self.call.cache_read_tokens = u["cache_read_input_tokens"]
                .as_u64()
                .or_else(|| {
                    u.pointer("/input_tokens_details/cached_tokens")
                        .and_then(Value::as_u64)
                })
                .or_else(|| {
                    u.pointer("/prompt_tokens_details/cached_tokens")
                        .and_then(Value::as_u64)
                })
                .or_else(|| u["cachedContentTokenCount"].as_u64())
                .unwrap_or(self.call.cache_read_tokens);
            self.call.cache_write_tokens = u["cache_creation_input_tokens"]
                .as_u64()
                .unwrap_or(self.call.cache_write_tokens);
            // Anthropic input_tokens excludes cache reads/writes; other dialects include them.
            if u.get("cache_read_input_tokens").is_some()
                || u.get("cache_creation_input_tokens").is_some()
            {
                self.uncached_input = u["input_tokens"].as_u64().or(self.uncached_input);
            }
            if let Some(uncached) = self.uncached_input {
                self.call.input_tokens =
                    uncached + self.call.cache_read_tokens + self.call.cache_write_tokens;
            }
        }
        if matches!(
            value["type"].as_str(),
            Some(
                "message_stop"
                    | "response.completed"
                    | "response.incomplete"
                    | "__douane_stream_done"
            )
        ) {
            self.complete = true;
        }
        if value["type"] == "error"
            || value["type"] == "response.failed"
            || value.get("error").is_some_and(|v| !v.is_null())
        {
            self.fail("upstream reported an error in its stream");
        }
    }
}
impl Drop for CallGuard {
    fn drop(&mut self) {
        self.ledger.in_flight.fetch_sub(1, Ordering::Relaxed);
        if !self.complete {
            self.call.error =
                Some("client disconnected or upstream response was interrupted".into());
        }
        self.call.latency_ms = self.start.elapsed().as_millis() as u64;
        self.ledger.record(std::mem::take(&mut self.call));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_account(provider: SubscriptionProvider) -> AccountCredential {
        serde_json::from_value(json!({
            "id":"fixture-account", "name":"fixture", "provider":provider,
            "access_token":"fixture-not-a-real-token", "enabled":true,
            "created_at":"2026-10-03T12:00:00Z", "updated_at":"2026-10-03T12:00:00Z"
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn anthropic_cache_write_then_read_counts_input_once_across_usage_events() {
        let ledger = Ledger::open(std::path::Path::new(":memory:")).unwrap();
        let account = fixture_account(SubscriptionProvider::Claude);
        let mut writing = CallGuard::new(ledger.clone(), &account, "claude-sonnet-4-6");
        let write_id = writing.call.id.clone();
        writing.observe(&json!({"type":"message_start","message":{"usage":{
            "input_tokens":13,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":7231
        }}}));
        assert_eq!(writing.call.input_tokens, 7244);
        writing.observe(&json!({"type":"message_delta","usage":{"output_tokens":22}}));
        writing.observe(&json!({"type":"message_delta","usage":{"output_tokens":22,"cache_creation_input_tokens":7231}}));
        assert_eq!(writing.call.input_tokens, 7244);
        writing.observe(&json!({"type":"message_stop"}));
        drop(writing);

        let mut reading = CallGuard::new(ledger.clone(), &account, "claude-sonnet-4-6");
        let read_id = reading.call.id.clone();
        reading.observe(&json!({"type":"message_start","message":{"usage":{
            "input_tokens":19,"output_tokens":0,"cache_read_input_tokens":7231,"cache_creation_input_tokens":0
        }}}));
        assert_eq!(reading.call.input_tokens, 7250);
        reading.observe(&json!({"type":"message_delta","usage":{"output_tokens":16}}));
        reading.observe(&json!({"type":"message_delta","usage":{"output_tokens":16,"cache_read_input_tokens":7231}}));
        assert_eq!(reading.call.input_tokens, 7250);
        reading.observe(&json!({"type":"message_stop"}));
        drop(reading);

        assert_eq!(ledger.requests.load(Ordering::Relaxed), 2);
        assert_eq!(ledger.errors.load(Ordering::Relaxed), 0);
        assert_eq!(ledger.input_tokens.load(Ordering::Relaxed), 14494);
        assert_eq!(ledger.output_tokens.load(Ordering::Relaxed), 38);
        assert_eq!(ledger.cache_read_tokens.load(Ordering::Relaxed), 7231);
        assert_eq!(ledger.cache_write_tokens.load(Ordering::Relaxed), 7231);
        assert_eq!(ledger.in_flight.load(Ordering::Relaxed), 0);
        let calls = ledger.calls(2).await;
        let written = calls.iter().find(|call| call.id == write_id).unwrap();
        let read = calls.iter().find(|call| call.id == read_id).unwrap();
        assert_eq!(
            (
                written.input_tokens,
                written.cache_read_tokens,
                written.cache_write_tokens
            ),
            (7244, 0, 7231)
        );
        assert_eq!(
            (
                read.input_tokens,
                read.cache_read_tokens,
                read.cache_write_tokens
            ),
            (7250, 7231, 0)
        );
    }

    #[tokio::test]
    async fn codex_input_total_already_includes_cache_reads_and_is_not_added_again() {
        let ledger = Ledger::open(std::path::Path::new(":memory:")).unwrap();
        let account = fixture_account(SubscriptionProvider::Codex);
        let mut call = CallGuard::new(ledger.clone(), &account, "gpt-fixture");
        let completed = json!({"type":"response.completed","response":{"usage":{
            "input_tokens":900,"output_tokens":40,"input_tokens_details":{"cached_tokens":800}
        }}});
        // Buffered collection can observe both the terminal event and its response.
        call.observe(&completed);
        call.observe(&completed["response"]);
        assert_eq!(call.call.input_tokens, 900);
        assert_eq!(call.call.cache_read_tokens, 800);
        assert_eq!(call.call.cache_write_tokens, 0);
        drop(call);
        assert_eq!(ledger.input_tokens.load(Ordering::Relaxed), 900);
        assert_eq!(ledger.cache_read_tokens.load(Ordering::Relaxed), 800);
        assert_eq!(ledger.requests.load(Ordering::Relaxed), 1);
        assert_eq!(ledger.errors.load(Ordering::Relaxed), 0);
        let recorded = ledger.calls(1).await;
        assert_eq!(recorded.len(), 1);
        assert_eq!(
            (
                recorded[0].input_tokens,
                recorded[0].output_tokens,
                recorded[0].cache_read_tokens
            ),
            (900, 40, 800)
        );
    }
}
