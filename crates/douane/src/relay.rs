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
    time::{Duration, Instant},
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
    let count_tokens = uri.path().ends_with("/messages/count_tokens");
    let from = if uri.path().ends_with("/messages") || count_tokens {
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
    // The Grok CLI repeats its model in a header, which gets the provider's id
    // as the body does: xAI does not know the gateway's prefix.
    let mut headers = headers;
    if headers.contains_key("x-grok-model-override") {
        if let Ok(named) = model.parse() {
            headers.insert("x-grok-model-override", named);
        }
    }
    let mut last_status = StatusCode::BAD_GATEWAY;
    // The last native refusal, returned as is when every account refused.
    let mut last_native: Option<Response> = None;
    // Google's reason for the last refusal, so a client sees more than a status.
    let mut last_reason: Option<String> = None;
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
        if count_tokens {
            if account.provider != SubscriptionProvider::Claude {
                return error(
                    StatusCode::NOT_FOUND,
                    "token counting is offered only by Claude accounts",
                );
            }
            url.push_str("/count_tokens");
        }
        let custom = translate::custom_tools(&value, from);
        let mut translated = match translate::request(&value, from, to) {
            Ok(v) => v,
            Err(e) => return error(StatusCode::BAD_REQUEST, &e),
        };
        let codex_sse =
            account.provider == SubscriptionProvider::Codex && !uri.path().ends_with("/compact");
        // Same dialect: the agent talks to its own provider and reads its
        // answers, limits and refusals exactly as a direct subscription would.
        let native = from == to;
        if codex_sse {
            translated["stream"] = json!(true);
            translated["store"] = json!(false);
            if translated.get("instructions").is_none() {
                translated["instructions"] = json!("");
            }
            // Codex's backend refuses a plain text input: "Input must be a list".
            if let Some(text) = translated["input"].as_str() {
                translated["input"] =
                    json!([{"role":"user","content":[{"type":"input_text","text":text}]}]);
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
        // The setting bounds each wait on the upstream, never the whole answer:
        // a long generation keeps streaming as long as its events keep coming.
        let idle = Duration::from_secs(state.config.read().await.settings.request_timeout_seconds);
        let response = loop {
            let send = state
                .client
                .post(&url)
                .headers(upstream_headers(&account, &headers))
                .body(bytes.clone())
                .send();
            let result = match tokio::time::timeout(idle, send).await {
                Ok(sent) => sent.map_err(|_| "upstream connection failed"),
                Err(_) => Err("upstream did not answer in time"),
            };
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
            Err(e) => {
                call.fail(e);
                let _ = state.accounts.cooldown(&account.id, 10, e).await;
                continue;
            }
        };
        last_status = response.status();
        call.call.status = last_status.as_u16();
        let passed = if native {
            passthrough_headers(response.headers())
        } else {
            HeaderMap::new()
        };
        if !last_status.is_success() {
            let advised = cooldown_seconds(response.headers(), unix_now());
            let (response, refusal) = if to == Dialect::Gemini && !native {
                let body = read_bounded(response, 64 * 1024, idle)
                    .await
                    .unwrap_or_default();
                (None, google_refusal(&body))
            } else {
                (Some(response), None)
            };
            let summary = match last_status {
                StatusCode::TOO_MANY_REQUESTS => "upstream quota or rate limit reached",
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                    "upstream authentication refused"
                }
                _ => "upstream request refused",
            };
            match &refusal {
                Some(refusal) => {
                    call.fail(&format!("{summary} ({})", refusal.reason));
                    last_reason = Some(refusal.reason.clone());
                }
                None => call.fail(summary),
            }
            if last_status == StatusCode::TOO_MANY_REQUESTS || last_status.is_server_error() {
                let wait = advised
                    .or_else(|| refusal.as_ref().and_then(|r| r.retry_seconds))
                    .unwrap_or(30);
                let _ = state
                    .accounts
                    .cooldown(&account.id, wait, "upstream temporarily unavailable")
                    .await;
                if let (true, Some(response)) = (native, response) {
                    last_native = native_refusal(last_status, passed, response, idle).await;
                }
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
            // A native refusal of the request itself (too long, invalid) is the
            // agent's to read and act on, compaction included; the account is fine.
            if let (true, Some(response)) = (native, response) {
                if let Some(refusal) = native_refusal(last_status, passed, response, idle).await {
                    return refusal;
                }
            } else {
                let _ = state
                    .accounts
                    .mark_error(&account.id, Some("upstream request refused".into()))
                    .await;
            }
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
        // Codex was asked for a stream above, and its backend can answer one
        // without a content type.
        let is_sse = codex_sse
            || response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.contains("text/event-stream"));
        if stream && is_sse {
            let mut streamed = stream_response(response, from, to, model, call, idle, custom);
            streamed.headers_mut().extend(passed);
            return streamed;
        }
        let result = if is_sse {
            collect_response(response, to, &mut call, idle).await
        } else {
            match read_bounded(response, 32 * 1024 * 1024, idle).await {
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
        let result = match translate::response(&result, to, from, &model, &custom) {
            Ok(v) => v,
            Err(e) => {
                call.fail(&e);
                return error(StatusCode::BAD_GATEWAY, &e);
            }
        };
        call.complete = true;
        let mut answer = Json(result).into_response();
        answer.headers_mut().extend(passed);
        return answer;
    }
    if let Some(refusal) = last_native {
        return refusal;
    }
    match last_reason {
        Some(reason) => error(
            last_status,
            &format!(
                "all accounts for this provider are unavailable; last upstream refusal: {reason}"
            ),
        ),
        None => error(
            last_status,
            "all accounts for this provider are unavailable; check their quotas and connection status",
        ),
    }
}

/// A Google (Gemini, Antigravity) refusal reduced to its enum fields. The
/// free-text message is dropped: it can name the project or the account.
#[derive(Debug, PartialEq)]
struct GoogleRefusal {
    /// `error.status`, then the first `details[].reason`, such as
    /// `RESOURCE_EXHAUSTED/MODEL_CAPACITY_EXHAUSTED`.
    reason: String,
    /// The delay Google advises (`RetryInfo.retryDelay` or `quotaResetDelay`).
    retry_seconds: Option<u64>,
}

fn google_refusal(body: &[u8]) -> Option<GoogleRefusal> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let error = value.get("error")?;
    let token = |v: &Value| {
        v.as_str()
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 64
                    && s.chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            })
            .map(str::to_owned)
    };
    let details = error["details"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut parts: Vec<String> = token(&error["status"]).into_iter().collect();
    parts.extend(details.iter().find_map(|d| token(&d["reason"])));
    if parts.is_empty() {
        return None;
    }
    let retry_seconds = details.iter().find_map(|d| {
        d["retryDelay"]
            .as_str()
            .or_else(|| {
                d.pointer("/metadata/quotaResetDelay")
                    .and_then(Value::as_str)
            })
            .and_then(google_duration)
    });
    Some(GoogleRefusal {
        reason: parts.join("/"),
        retry_seconds,
    })
}

/// Seconds in a Google duration: `2.5s`, `1h2m3.4s`, `750ms`; rounded up.
fn google_duration(text: &str) -> Option<u64> {
    let mut total = 0.0;
    let mut number = String::new();
    let mut chars = text.trim().chars().peekable();
    let mut seen = false;
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() || c == '.' {
            number.push(c);
            continue;
        }
        let value: f64 = number.parse().ok()?;
        number.clear();
        let unit = match (c, chars.peek()) {
            ('m', Some('s')) => {
                chars.next();
                0.001
            }
            ('h', _) => 3600.0,
            ('m', _) => 60.0,
            ('s', _) => 1.0,
            _ => return None,
        };
        total += value * unit;
        seen = true;
    }
    (seen && number.is_empty() && total.is_finite()).then(|| total.ceil() as u64)
}

pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// How long a refusing account rests, from what its provider says: an
/// explicit `retry-after`, else the reset of the exhausted subscription
/// window. Retrying an exhausted window every 30 seconds only adds refusals
/// to the account's record.
pub(crate) fn cooldown_seconds(headers: &HeaderMap, now: u64) -> Option<u64> {
    let number = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
    };
    if let Some(seconds) = number("retry-after") {
        return Some(seconds.ceil() as u64);
    }
    if headers
        .get("anthropic-ratelimit-unified-status")
        .and_then(|v| v.to_str().ok())
        == Some("rejected")
        && let Some(reset) = number("anthropic-ratelimit-unified-reset")
    {
        return Some((reset as u64).saturating_sub(now).max(1));
    }
    ["primary", "secondary"].iter().find_map(|window| {
        let used = number(&format!("x-codex-{window}-used-percent"))?;
        (used >= 100.0)
            .then(|| number(&format!("x-codex-{window}-reset-after-seconds")))
            .flatten()
            .map(|seconds| seconds.ceil() as u64)
    })
}

/// Response headers an agent reads from its own provider: rate-limit state
/// (Claude's unified limits drive its fast-mode fallback and usage warnings,
/// Codex's drive its status line) and request ids. No cookie, no credential.
pub(crate) fn passthrough_headers(upstream: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in upstream {
        let name_text = name.as_str();
        if name_text.starts_with("anthropic-ratelimit-")
            || name_text.starts_with("x-codex-")
            || name_text.starts_with("x-ratelimit-")
            || matches!(name_text, "request-id" | "x-request-id" | "retry-after")
        {
            out.append(name.clone(), value.clone());
        }
    }
    out
}

/// The provider's own JSON refusal with its status and limit headers, or
/// nothing when its body is not a bounded JSON document.
async fn native_refusal(
    status: StatusCode,
    headers: HeaderMap,
    response: reqwest::Response,
    idle: Duration,
) -> Option<Response> {
    let body = read_bounded(response, 256 * 1024, idle).await.ok()?;
    serde_json::from_slice::<Value>(&body).ok()?;
    let mut refusal = (status, Body::from(body)).into_response();
    refusal.headers_mut().extend(headers);
    refusal.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    Some(refusal)
}

/// The upstream went quiet for longer than the request timeout.
const STALLED: &str = "upstream stream stalled";

/// The next piece of an upstream body. Silence longer than `idle` ends the
/// body as `STALLED`; a transport failure ends it as `interrupted`.
async fn next_chunk<S>(
    body: &mut S,
    idle: Duration,
    interrupted: &str,
) -> Option<Result<Bytes, String>>
where
    S: futures_util::Stream<Item = reqwest::Result<Bytes>> + Unpin,
{
    match tokio::time::timeout(idle, body.next()).await {
        Ok(Some(Ok(chunk))) => Some(Ok(chunk)),
        Ok(Some(Err(_))) => Some(Err(interrupted.into())),
        Ok(None) => None,
        Err(_) => Some(Err(STALLED.into())),
    }
}

pub(crate) async fn read_bounded(
    response: reqwest::Response,
    cap: usize,
    idle: Duration,
) -> Result<Vec<u8>, String> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = next_chunk(&mut stream, idle, "upstream body interrupted").await {
        let chunk = chunk?;
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
    idle: Duration,
) -> Result<Value, String> {
    let mut decoder = Frames::default();
    let mut upstream = response.bytes_stream();
    let mut collected = Collected::default();
    while let Some(chunk) = next_chunk(&mut upstream, idle, "upstream stream interrupted").await {
        for frame in decoder.feed(&chunk?)? {
            collected.frame(&frame, to, call)?;
        }
    }
    for frame in decoder.finish()? {
        collected.frame(&frame, to, call)?;
    }
    let mut result = collected
        .result
        .ok_or("upstream stream closed before completing its response")?;
    // Codex sends its items as `response.output_item.done` and completes with
    // an empty `output`; the finished items are the answer.
    let empty = result["output"].as_array().is_none_or(Vec::is_empty);
    if empty && !collected.items.is_empty() {
        collected.items.sort_by_key(|(index, _)| *index);
        result["output"] =
            Value::Array(collected.items.into_iter().map(|(_, item)| item).collect());
    }
    Ok(result)
}

#[derive(Default)]
struct Collected {
    result: Option<Value>,
    items: Vec<(u64, Value)>,
}

impl Collected {
    fn frame(&mut self, frame: &Value, to: Dialect, call: &mut CallGuard) -> Result<(), String> {
        call.observe(frame);
        if frame["type"] == "response.failed" || frame["type"] == "error" {
            return Err("upstream reported a stream error".into());
        }
        if to != Dialect::Responses {
            return Ok(());
        }
        match frame["type"].as_str() {
            Some("response.output_item.done") if frame["item"].is_object() => {
                let index = frame["output_index"]
                    .as_u64()
                    .unwrap_or(self.items.len() as u64);
                self.items.push((index, frame["item"].clone()));
            }
            Some("response.completed" | "response.incomplete") => {
                self.result = Some(frame["response"].clone());
            }
            _ => {}
        }
        Ok(())
    }
}

fn stream_response(
    response: reqwest::Response,
    from: Dialect,
    to: Dialect,
    model: String,
    call: CallGuard,
    idle: Duration,
    custom: std::collections::BTreeSet<String>,
) -> Response {
    let bridge = if from == to {
        None
    } else {
        Some(Bridge::new(to, from, &model).with_custom_tools(custom))
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
        move |(mut upstream, mut frames, mut bridge, mut call, mut done)| async move {
            if done {
                return None;
            }
            loop {
                let next = next_chunk(&mut upstream, idle, "upstream stream interrupted").await;
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
                    Some(Err(e)) => Err(e),
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

    #[test]
    fn an_exhausted_window_rests_until_the_provider_reset() {
        let headers = |pairs: &[(&'static str, &'static str)]| {
            let mut map = HeaderMap::new();
            for (name, value) in pairs {
                map.insert(*name, value.parse().unwrap());
            }
            map
        };
        assert_eq!(
            cooldown_seconds(&headers(&[("retry-after", "12")]), 0),
            Some(12)
        );
        let claude = headers(&[
            ("anthropic-ratelimit-unified-status", "rejected"),
            ("anthropic-ratelimit-unified-reset", "1000"),
        ]);
        assert_eq!(cooldown_seconds(&claude, 400), Some(600));
        let codex = headers(&[
            ("x-codex-primary-used-percent", "62"),
            ("x-codex-primary-reset-after-seconds", "100"),
            ("x-codex-secondary-used-percent", "100"),
            ("x-codex-secondary-reset-after-seconds", "7200"),
        ]);
        assert_eq!(cooldown_seconds(&codex, 0), Some(7200));
        assert_eq!(
            cooldown_seconds(
                &headers(&[("anthropic-ratelimit-unified-status", "allowed")]),
                0
            ),
            None
        );
    }

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

    #[test]
    fn google_refusal_keeps_enum_fields_and_retry_delay_only() {
        let body = br#"{"error":{"code":429,"message":"No capacity on project secret-project-42 for you@example.com","status":"RESOURCE_EXHAUSTED",
            "details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"MODEL_CAPACITY_EXHAUSTED","domain":"cloudcode-pa.googleapis.com","metadata":{"model":"gemini-3.8-flash-high"}},
            {"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"2.5s"}]}}"#;
        let refusal = super::google_refusal(body).unwrap();
        assert_eq!(
            refusal.reason,
            "RESOURCE_EXHAUSTED/MODEL_CAPACITY_EXHAUSTED"
        );
        assert_eq!(refusal.retry_seconds, Some(3));
        let quota = br#"{"error":{"status":"RESOURCE_EXHAUSTED","details":[{"reason":"QUOTA_EXHAUSTED","metadata":{"quotaResetDelay":"1h2m3.4s"}}]}}"#;
        assert_eq!(
            super::google_refusal(quota).unwrap().retry_seconds,
            Some(3724)
        );
        assert_eq!(super::google_duration("750ms"), Some(1));
        assert_eq!(super::google_duration("soon"), None);
        let free_text = br#"{"error":{"status":"Project secret-project-42 is blocked"}}"#;
        assert_eq!(super::google_refusal(free_text), None);
        assert_eq!(super::google_refusal(b"not json"), None);
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
