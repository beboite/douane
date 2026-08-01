//! The relay itself.
//!
//! An agent sends a normal Anthropic Messages request with a virtual key.
//! douane checks the key, decides which upstream should answer, looks at what
//! is about to leave, swaps in the real credential, and streams the reply
//! straight back. The agent never learns which provider answered, and never
//! holds a credential that works anywhere else.
//!
//! The body is forwarded byte for byte. Rewriting it would mean tracking every
//! wire feature Anthropic ships (signed thinking blocks, cache breakpoints, the
//! exact SSE event order), and getting one of them subtly wrong produces bugs
//! that look like model misbehaviour. So: read the few fields needed to route,
//! forward the rest untouched.
//!
//! **Placeholders are never substituted here.** `__DOUANE_NS_KEY__` exists so an
//! agent can carry a credential's name through a conversation; expanding one on
//! the way to a provider would hand that provider the value, which is the exact
//! thing this program is built to prevent. A placeholder in an outgoing body is
//! harmless and stays as it is.

use std::sync::{Arc, RwLock};
use std::time::Instant;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use douane_vault::{Secret, SecretBackend};
use futures_util::StreamExt as _;

use crate::audit::{Audit, Finish, NewAlert, Start, fingerprint};
use crate::config::{Action, Config, Pricing, Provider, Trust};
use crate::scrub::Scrubber;
use crate::usage::{Mode, Tap};

pub struct AppState {
    /// The configuration as written in the TOML. Never mutated.
    pub base: Config,
    /// The base with the dashboard's route overrides applied. Swapped whole, so
    /// a request either sees the old table or the new one and never a half-way
    /// state.
    live: RwLock<Arc<Config>>,
    pub vault: Box<dyn SecretBackend>,
    pub scrubber: Scrubber,
    pub http: reqwest::Client,
    pub audit: Audit,
    /// Guards the dashboard. Absent means the dashboard is not served at all.
    pub admin_token: Option<Secret>,
}

impl AppState {
    pub fn new(
        base: Config,
        vault: Box<dyn SecretBackend>,
        scrubber: Scrubber,
        http: reqwest::Client,
        audit: Audit,
        admin_token: Option<Secret>,
    ) -> anyhow::Result<Self> {
        let live = RwLock::new(Arc::new(base.with_overrides(&audit.overrides()?)));
        Ok(Self {
            base,
            live,
            vault,
            scrubber,
            http,
            audit,
            admin_token,
        })
    }

    /// The table in force right now. Cloning an `Arc` is the whole cost, so
    /// taking a snapshot per request is cheaper than holding a lock across one.
    pub fn config(&self) -> Arc<Config> {
        self.live.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Rebuild the live table from the base plus whatever the database now
    /// holds. Called after the dashboard changes a route.
    pub fn refresh_routes(&self) -> anyhow::Result<()> {
        let next = Arc::new(self.base.with_overrides(&self.audit.overrides()?));
        *self.live.write().unwrap_or_else(|e| e.into_inner()) = next;
        Ok(())
    }
}

/// Headers douane must not pass through.
///
/// `host` and the hop-by-hop set would be wrong for the new destination.
/// `x-api-key` and `authorization` carry the *virtual* key, which upstream must
/// never see: that is the whole point of the exercise.
///
/// `accept-encoding` goes for a different reason. A client that asks for gzip
/// gets a gzipped answer, and douane then relays bytes it cannot read: the
/// token counter sees compressed noise and records nothing, silently, on every
/// call. A broker that cannot read what passes through it is not brokering. The
/// cost is bandwidth on the hop douane pays for, which is not the scarce thing
/// here.
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
    "accept-encoding",
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

pub fn json_error(status: StatusCode, kind: &str, message: &str) -> Response {
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
    let config = state.config();

    // 1. Who is calling.
    let Some(presented) = presented_key(&headers) else {
        return json_error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "no virtual key presented",
        );
    };
    let Some((client_name, client)) = config.clients.iter().find(|(_, c)| {
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

    let chain = config.chain(&peeked.model);
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
        let route = &config.routes[route_name];
        let provider = &config.providers[&route.provider];

        if !client.routes.is_empty() && !client.routes.iter().any(|r| r == route_name) {
            continue;
        }
        if let Some(cap) = client.max_trust
            && provider.trust > cap
        {
            continue;
        }

        // 4. Look at what is about to leave, for this destination's trust level.
        let (hits, verdict) = state.scrubber.inspect(&body, provider.trust);
        for h in &hits {
            let _ = state.audit.alert(&NewAlert {
                call_id: None,
                client: client_name,
                provider: &route.provider,
                trust: trust_label(provider.trust),
                kind: h.kind,
                label: &h.label,
                action: match verdict {
                    Action::Block => "block",
                    Action::Warn => "warn",
                    Action::Allow => "allow",
                },
            });
            tracing::warn!(
                client = client_name,
                provider = route.provider,
                kind = h.kind,
                label = h.label,
                "credential material heading upstream"
            );
        }
        if verdict == Action::Block {
            // Deliberately not falling through to the next route. Retrying the
            // same body against a provider with a laxer policy would be a way of
            // shopping for somewhere the leak is tolerated.
            let what: Vec<&str> = hits.iter().map(|h| h.label.as_str()).collect();
            return json_error(
                StatusCode::FORBIDDEN,
                "permission_error",
                &format!(
                    "douane refused to forward this request: it carries {} ({}). \
                     Nothing was sent upstream.",
                    what.join(", "),
                    hits.first().map(|h| h.kind).unwrap_or("unknown")
                ),
            );
        }

        let model_sent = route.model.as_deref().unwrap_or(&peeked.model).to_owned();
        let call_id = state
            .audit
            .begin(&Start {
                client: client_name.clone(),
                route: route_name.to_owned(),
                provider: route.provider.clone(),
                model_requested: peeked.model.clone(),
                model_sent: model_sent.clone(),
                trust: trust_label(provider.trust),
                agent_fingerprint: peeked.agent_fingerprint.clone(),
                thread_fingerprint: peeked.thread_fingerprint.clone(),
                streamed: peeked.stream,
                bytes_up: body.len(),
            })
            .unwrap_or(-1);

        match forward(
            &state,
            provider,
            &body,
            &headers,
            &model_sent,
            peeked.stream,
            call_id,
            started,
        )
        .await
        {
            Ok((status, response)) => {
                // Only a server-side failure is worth trying the next provider
                // for. A 400 means the request itself is wrong and every
                // upstream will say so, and a 401 upstream means douane's own
                // credential is bad, which retrying elsewhere would only hide.
                if status.as_u16() < 500 {
                    return response;
                }
                last = Some(response);
            }
            Err(e) => {
                let _ = state.audit.finish(
                    call_id,
                    &Finish {
                        status: StatusCode::BAD_GATEWAY.as_u16(),
                        duration_ms: started.elapsed().as_millis() as u64,
                        error: Some(e.to_string()),
                        ..Finish::default()
                    },
                );
                last = Some(json_error(
                    StatusCode::BAD_GATEWAY,
                    "api_error",
                    &e.to_string(),
                ));
            }
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

#[allow(clippy::too_many_arguments, reason = "one call site, all of it needed")]
async fn forward(
    state: &Arc<AppState>,
    provider: &Provider,
    body: &[u8],
    headers: &HeaderMap,
    model_sent: &str,
    streaming: bool,
    call_id: i64,
    started: Instant,
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
    let mut req = state.http.post(&url).body(outgoing);

    // A total timeout is right for a buffered answer and wrong for a streamed
    // one: a long generation legitimately takes minutes. Streams are held to
    // the client-wide read timeout instead, which fires on an upstream that has
    // gone quiet rather than on one that is merely slow.
    if !streaming {
        req = req.timeout(std::time::Duration::from_secs(provider.timeout_secs));
    }

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

    let sse = upstream
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"));

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

    // Streamed straight through, with a tap on the way past. Buffering would
    // break SSE, and buffering a long generation would also mean holding the
    // whole answer in memory for no reason.
    let tapped = tap_stream(
        upstream.bytes_stream(),
        Ledger {
            state: state.clone(),
            call_id,
            started,
            status: status.as_u16(),
            pricing: provider.pricing,
            tap: Tap::new(if sse { Mode::Sse } else { Mode::Json }),
        },
    );
    Ok((status, out.body(Body::from_stream(tapped))?.into_response()))
}

/// Everything needed to close a call's row, carried alongside its body.
///
/// The write happens in `Drop`, which covers both endings that matter: the
/// stream running out, and the client hanging up half way through a generation.
/// The second is common (a cancelled turn) and would otherwise leave a row with
/// no counts at all, while the tokens have still been billed upstream.
struct Ledger {
    state: Arc<AppState>,
    call_id: i64,
    started: Instant,
    status: u16,
    pricing: Option<Pricing>,
    tap: Tap,
}

impl Drop for Ledger {
    fn drop(&mut self) {
        self.tap.finish();
        let usage = self.tap.usage();
        let cost = self.pricing.as_ref().and_then(|p| usage.cost(p));
        let _ = self.state.audit.finish(
            self.call_id,
            &Finish {
                status: self.status,
                duration_ms: self.started.elapsed().as_millis() as u64,
                bytes_down: self.tap.bytes,
                usage,
                cost,
                error: None,
            },
        );
    }
}

/// Pass every chunk through untouched, showing each one to the ledger's tap.
fn tap_stream<S, E>(
    inner: S,
    ledger: Ledger,
) -> impl futures_util::Stream<Item = Result<bytes::Bytes, std::io::Error>>
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, E>> + Unpin,
    E: std::error::Error + Send + Sync + 'static,
{
    struct St<S> {
        inner: S,
        ledger: Ledger,
        done: bool,
    }

    futures_util::stream::unfold(
        St {
            inner,
            ledger,
            done: false,
        },
        |mut st| async move {
            if st.done {
                return None;
            }
            match st.inner.next().await {
                Some(Ok(chunk)) => {
                    st.ledger.tap.feed(&chunk);
                    Some((Ok(chunk), st))
                }
                Some(Err(e)) => {
                    // One error, then stop. An upstream that keeps yielding
                    // errors would otherwise spin here forever.
                    st.done = true;
                    Some((Err(std::io::Error::other(e)), st))
                }
                None => None,
            }
        },
    )
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
