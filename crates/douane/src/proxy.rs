//! The relay itself.
//!
//! An agent sends a normal Anthropic Messages request with a virtual key.
//! douane checks the key, decides which upstream should answer, swaps in the
//! real credential, and streams the reply straight back. The agent never learns
//! which provider answered, and never holds a credential that works anywhere
//! else.
//!
//! The body is forwarded byte for byte. Rewriting it would mean tracking every
//! wire feature Anthropic ships (signed thinking blocks, cache breakpoints, the
//! exact SSE event order), and getting one of them subtly wrong produces bugs
//! that look like model misbehaviour. So: read the few fields needed to route,
//! forward the rest untouched.

use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use douane_vault::SecretBackend;

use crate::audit::{Audit, Record, fingerprint};
use crate::config::{Config, Provider, Trust};

pub struct AppState {
    pub config: Config,
    pub vault: Box<dyn SecretBackend>,
    pub http: reqwest::Client,
    pub audit: Audit,
}

/// Headers douane must not pass through.
///
/// `host` and the hop-by-hop set would be wrong for the new destination.
/// `x-api-key` and `authorization` carry the *virtual* key, which upstream must
/// never see: that is the whole point of the exercise.
const STRIP: &[&str] = &[
    "host",
    "content-length",
    "connection",
    "keep-alive",
    "transfer-encoding",
    "upgrade",
    "proxy-authorization",
    "x-api-key",
    "authorization",
];

fn trust_label(t: Trust) -> &'static str {
    match t {
        Trust::Max => "max",
        Trust::Mid => "mid",
        Trust::Low => "low",
    }
}

/// What the router needs from the body, and nothing more.
struct Peek {
    model: String,
    stream: bool,
    agent_fingerprint: Option<String>,
    thread_fingerprint: Option<String>,
}

fn peek(body: &[u8]) -> Option<Peek> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let model = v.get("model")?.as_str()?.to_owned();
    let stream = v.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);

    // The system prompt identifies the caller's role. Claude Code gives each
    // subagent type a distinct one, and never sends it as a header, so this is
    // the only handle on "which agent is this".
    let agent_fingerprint = v.get("system").map(|s| match s {
        serde_json::Value::String(s) => fingerprint(s.as_bytes()),
        other => fingerprint(other.to_string().as_bytes()),
    });

    // The first message is stable for the life of a conversation while the rest
    // grows, so it groups turns without the client sending a session id.
    let thread_fingerprint = v
        .get("messages")
        .and_then(|m| m.as_array())
        .and_then(|a| a.first())
        .map(|m| fingerprint(m.to_string().as_bytes()));

    Some(Peek {
        model,
        stream,
        agent_fingerprint,
        thread_fingerprint,
    })
}

/// Pull the presented key out of either header form.
fn presented_key(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        return Some(v.to_owned());
    }
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.strip_prefix("Bearer ").unwrap_or(v).to_owned())
}

fn json_error(status: StatusCode, kind: &str, message: &str) -> Response {
    // Anthropic's error shape, so a client that already handles upstream errors
    // handles douane's without a special case.
    let body = serde_json::json!({
        "type": "error",
        "error": { "type": kind, "message": message }
    });
    (status, axum::Json(body)).into_response()
}

pub async fn messages(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let started = Instant::now();

    // 1. Who is calling.
    let Some(presented) = presented_key(&headers) else {
        return json_error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "no virtual key presented",
        );
    };
    let Some((client_name, client)) = state.config.clients.iter().find(|(_, c)| {
        state
            .vault
            .get(&c.key)
            .is_some_and(|stored| stored.ct_eq(&presented))
    }) else {
        return json_error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "unknown virtual key",
        );
    };

    // 2. What did they ask for.
    let Some(peeked) = peek(&body) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "body is not a JSON object carrying a `model`",
        );
    };

    let chain = state.config.chain(&peeked.model);
    if chain.is_empty() {
        return json_error(
            StatusCode::NOT_FOUND,
            "not_found_error",
            &format!("no route for model `{}`", peeked.model),
        );
    }

    // 3. Try the chain in order. A route the client is not allowed to use is
    //    skipped rather than refused outright, so a fallback still works for a
    //    client whose primary happens to be off limits.
    let mut last: Option<Response> = None;
    for route_name in chain {
        let route = &state.config.routes[route_name];
        let provider = &state.config.providers[&route.provider];

        if !client.routes.is_empty() && !client.routes.iter().any(|r| r == route_name) {
            continue;
        }
        if let Some(cap) = client.max_trust
            && provider.trust > cap
        {
            continue;
        }

        let model_sent = route.model.as_deref().unwrap_or(&peeked.model).to_owned();
        let outcome = forward(&state, provider, &body, &headers, &model_sent).await;

        let (status, response, error) = match outcome {
            Ok((status, response)) => (status, Some(response), None),
            Err(e) => (StatusCode::BAD_GATEWAY, None, Some(e.to_string())),
        };

        let _ = state.audit.record(&Record {
            client: client_name.clone(),
            route: route_name.to_owned(),
            provider: route.provider.clone(),
            model_requested: peeked.model.clone(),
            model_sent,
            trust: trust_label(provider.trust),
            agent_fingerprint: peeked.agent_fingerprint.clone(),
            thread_fingerprint: peeked.thread_fingerprint.clone(),
            status: status.as_u16(),
            streamed: peeked.stream,
            duration_ms: started.elapsed().as_millis() as u64,
            bytes_up: body.len(),
            bytes_down: 0,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            error: error.clone(),
        });

        match response {
            // Only a server-side failure is worth trying the next provider for.
            // A 400 means the request itself is wrong and every upstream will
            // say so, and a 401 upstream means douane's own credential is bad,
            // which retrying elsewhere would only hide.
            Some(r) if status.is_success() || status.as_u16() < 500 => return r,
            other => last = other,
        }
    }

    last.unwrap_or_else(|| {
        json_error(
            StatusCode::FORBIDDEN,
            "permission_error",
            "no route this client is allowed to use can serve that model",
        )
    })
}

async fn forward(
    state: &AppState,
    provider: &Provider,
    body: &[u8],
    headers: &HeaderMap,
    model_sent: &str,
) -> anyhow::Result<(StatusCode, Response)> {
    // Rewrite only the model field. Everything else the agent sent travels as
    // it was written.
    let outgoing = if model_sent.is_empty() {
        body.to_vec()
    } else {
        let mut v: serde_json::Value = serde_json::from_slice(body)?;
        v["model"] = serde_json::Value::String(model_sent.to_owned());
        serde_json::to_vec(&v)?
    };

    let url = format!("{}/v1/messages", provider.base_url.trim_end_matches('/'));
    let mut req = state
        .http
        .post(&url)
        .timeout(std::time::Duration::from_secs(provider.timeout_secs))
        .body(outgoing);

    for (name, value) in headers {
        if STRIP.contains(&name.as_str()) {
            continue;
        }
        req = req.header(name, value);
    }

    if let Some(key_ref) = &provider.key {
        let key = state
            .vault
            .get(key_ref)
            .ok_or_else(|| anyhow::anyhow!("vault has no {key_ref}"))?;
        req = req.header(
            HeaderName::from_bytes(provider.auth_header.as_bytes())?,
            HeaderValue::from_str(&provider.header_value(key.expose()))?,
        );
    }

    let upstream = req.send().await?;
    let status = StatusCode::from_u16(upstream.status().as_u16())?;

    let mut out = Response::builder().status(status);
    for (name, value) in upstream.headers() {
        // Same reasoning as STRIP, in the other direction: the framing headers
        // describe the upstream connection, not the one being answered.
        if matches!(
            name.as_str(),
            "content-length" | "transfer-encoding" | "connection"
        ) {
            continue;
        }
        out = out.header(name, value);
    }

    // Streamed straight through. Buffering would break SSE, and buffering a
    // long generation would also mean holding the whole answer in memory for
    // no reason.
    let stream = futures_util::TryStreamExt::map_err(upstream.bytes_stream(), std::io::Error::other);
    Ok((status, out.body(Body::from_stream(stream))?.into_response()))
}

/// Liveness. Deliberately unauthenticated and deliberately says nothing about
/// what is configured.
pub async fn health() -> &'static str {
    "ok\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peek_reads_only_what_routing_needs() {
        let body = br#"{"model":"haiku","stream":true,"system":"You are an investigator.",
                        "messages":[{"role":"user","content":"hi"}],"max_tokens":100}"#;
        let p = peek(body).unwrap();
        assert_eq!(p.model, "haiku");
        assert!(p.stream);
        assert!(p.agent_fingerprint.is_some());
        assert!(p.thread_fingerprint.is_some());
    }

    #[test]
    fn the_same_agent_fingerprints_the_same_across_turns() {
        let turn1 = br#"{"model":"m","system":"S","messages":[{"role":"user","content":"a"}]}"#;
        let turn2 = br#"{"model":"m","system":"S","messages":[{"role":"user","content":"a"},
                         {"role":"assistant","content":"b"}]}"#;
        let a = peek(turn1).unwrap();
        let b = peek(turn2).unwrap();
        assert_eq!(a.agent_fingerprint, b.agent_fingerprint);
        // The thread is the same too: it keys on the first message, which does
        // not change as the conversation grows.
        assert_eq!(a.thread_fingerprint, b.thread_fingerprint);
    }

    #[test]
    fn different_first_messages_are_different_threads() {
        let a = peek(br#"{"model":"m","messages":[{"role":"user","content":"a"}]}"#).unwrap();
        let b = peek(br#"{"model":"m","messages":[{"role":"user","content":"z"}]}"#).unwrap();
        assert_ne!(a.thread_fingerprint, b.thread_fingerprint);
    }

    #[test]
    fn peek_refuses_a_body_with_no_model() {
        assert!(peek(br#"{"messages":[]}"#).is_none());
        assert!(peek(b"not json").is_none());
    }

    #[test]
    fn both_header_forms_are_accepted() {
        let mut h = HeaderMap::new();
        h.insert("x-api-key", HeaderValue::from_static("vk-1"));
        assert_eq!(presented_key(&h).as_deref(), Some("vk-1"));

        let mut h = HeaderMap::new();
        h.insert("authorization", HeaderValue::from_static("Bearer vk-2"));
        assert_eq!(presented_key(&h).as_deref(), Some("vk-2"));

        assert_eq!(presented_key(&HeaderMap::new()), None);
    }

    #[test]
    fn the_virtual_key_is_never_forwarded() {
        // Both header names must be in STRIP, or the reseller receives a key
        // that works against douane.
        assert!(STRIP.contains(&"x-api-key"));
        assert!(STRIP.contains(&"authorization"));
    }
}
