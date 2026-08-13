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
//! forward the rest untouched. Two exceptions, both deliberate: `model` is
//! rewritten to whatever the route says, and a verdict of
//! [`Action::Substitute`] rewrites vault values into their placeholders, which
//! is the point of that verdict.
//!
//! **Placeholders are never expanded here, in either direction.**
//! `__DOUANE_NS_KEY__` exists so an agent can carry a credential's name through
//! a conversation; expanding one on the way to a provider would hand that
//! provider the value, which is the exact thing this program is built to
//! prevent. The substitution above only ever turns a value *into* a
//! placeholder, never the reverse.

use std::sync::{Arc, RwLock};
use std::time::Instant;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use douane_vault::{Secret, SecretBackend};
use futures_util::StreamExt as _;

use crate::audit::{Audit, Finish, NewAlert, Start, fingerprint};
use crate::config::{Action, Config, Provider, Trust};
use crate::scrub::Scrubber;
use crate::usage::{Mode, Tap};
use crate::wire::{self, Wire};

pub struct AppState {
    /// The configuration as written in the TOML. Never mutated.
    pub base: Config,
    /// The base with the dashboard's route overrides applied. Swapped whole, so
    /// a request either sees the old table or the new one and never a half-way
    /// state.
    live: RwLock<Arc<Config>>,
    /// Behind a lock because the dashboard can deposit and withdraw entries
    /// while the broker serves. Private: the two accessors below are the only
    /// way in, which keeps the places a value can be reached countable.
    vault: RwLock<Box<dyn SecretBackend>>,
    /// Rebuilt from the vault on every write, never edited in place. The
    /// scrubber cannot recognise a value leaving unless it has been shown that
    /// value, so a credential deposited through the dashboard would travel
    /// unwatched until the next restart if this were built once at startup.
    scrubber: RwLock<Arc<Scrubber>>,
    pub http: reqwest::Client,
    pub audit: Audit,
}

impl AppState {
    pub fn new(
        base: Config,
        vault: Box<dyn SecretBackend>,
        http: reqwest::Client,
        audit: Audit,
    ) -> anyhow::Result<Self> {
        let live = RwLock::new(Arc::new(
            base.with_overrides(&audit.overrides()?)
                .with_models(&audit.models()?),
        ));
        let scrubber = RwLock::new(Arc::new(Scrubber::build(vault.as_ref(), base.scrub)));
        Ok(Self {
            base,
            live,
            vault: RwLock::new(vault),
            scrubber,
            http,
            audit,
        })
    }

    /// The table in force right now. Cloning an `Arc` is the whole cost, so
    /// taking a snapshot per request is cheaper than holding a lock across one.
    pub fn config(&self) -> Arc<Config> {
        self.live.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Rebuild the live table from the base plus whatever the database now
    /// holds. Called after the dashboard changes a route or switches a model on.
    ///
    /// The order is the guarantee: the file's routes come first, catalogue
    /// entries are added around them, and an override edits what is already
    /// there. So a model switched on can add a name, and never take one over.
    pub fn refresh_routes(&self) -> anyhow::Result<()> {
        let next = Arc::new(
            self.base
                .with_overrides(&self.audit.overrides()?)
                .with_models(&self.audit.models()?),
        );
        *self.live.write().unwrap_or_else(|e| e.into_inner()) = next;
        Ok(())
    }

    /// A snapshot of the outbound scrubber. Taken per request for the same
    /// reason the config is: a lock held across an upstream call would stall
    /// every writer for as long as a model takes to answer.
    pub fn scrubber(&self) -> Arc<Scrubber> {
        self.scrubber
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// One credential, copied out of the vault.
    ///
    /// A copy rather than a borrow because the guard must not be held across an
    /// `await`, and because a borrow would have to outlive the lock to be
    /// useful. This and [`Self::authenticate`] are the only two ways a value
    /// leaves the vault at runtime, which is what makes them worth grepping.
    pub fn secret(&self, r: &douane_vault::SecretRef) -> Option<Secret> {
        self.vault
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(r)
            .cloned()
    }

    /// Which client presented this key, if any.
    ///
    /// The comparison is constant time and happens under the read lock, so no
    /// copy of a virtual key is made to answer the question.
    pub fn authenticate<'a>(
        &self,
        config: &'a Config,
        presented: &str,
    ) -> Option<(&'a String, &'a crate::config::Client)> {
        let vault = self.vault.read().unwrap_or_else(|e| e.into_inner());
        config.clients.iter().find(|(_, c)| {
            vault
                .get(&c.key)
                .is_some_and(|stored| stored.ct_eq(presented))
        })
    }

    /// The vault as names. [`douane_vault::EntryInfo`] has no field a value
    /// could be put in, so this cannot leak one however it is called.
    pub fn vault_list(&self) -> Vec<douane_vault::EntryInfo> {
        self.vault.read().unwrap_or_else(|e| e.into_inner()).list()
    }

    /// Store an entry, flush the file, and re-teach the scrubber.
    ///
    /// The three happen under one write lock so no request can observe a vault
    /// that holds a value the scrubber has not been shown.
    pub fn vault_put(
        &self,
        r: &douane_vault::SecretRef,
        entry: douane_vault::Entry,
    ) -> Result<(), douane_vault::VaultError> {
        let mut vault = self.vault.write().unwrap_or_else(|e| e.into_inner());
        vault.put(r, entry);
        vault.persist()?;
        *self.scrubber.write().unwrap_or_else(|e| e.into_inner()) =
            Arc::new(Scrubber::build(vault.as_ref(), self.base.scrub));
        Ok(())
    }

    /// Read one entry back out to rewrite it with a new note. The value never
    /// leaves this function.
    pub fn vault_renote(
        &self,
        r: &douane_vault::SecretRef,
        note: Option<String>,
    ) -> Result<bool, douane_vault::VaultError> {
        let mut vault = self.vault.write().unwrap_or_else(|e| e.into_inner());
        let Some(value) = vault.get(r).cloned() else {
            return Ok(false);
        };
        let mut entry = douane_vault::Entry::new(value);
        entry.note = note;
        vault.put(r, entry);
        vault.persist()?;
        Ok(true)
    }

    pub fn vault_delete(
        &self,
        r: &douane_vault::SecretRef,
    ) -> Result<bool, douane_vault::VaultError> {
        let mut vault = self.vault.write().unwrap_or_else(|e| e.into_inner());
        if !vault.delete(r) {
            return Ok(false);
        }
        vault.persist()?;
        *self.scrubber.write().unwrap_or_else(|e| e.into_inner()) =
            Arc::new(Scrubber::build(vault.as_ref(), self.base.scrub));
        Ok(true)
    }

    /// The token that guards the dashboard, read fresh from the vault.
    ///
    /// Fresh rather than cached at startup so that rotating it through the
    /// dashboard takes effect on the next request. Absent means no dashboard is
    /// served at all, rather than one served openly.
    pub fn admin_token(&self) -> Option<Secret> {
        self.secret(self.base.server.admin_key.as_ref()?)
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

/// What the caller says it is. A hint and never a credential: it names the
/// harness on the live screen, where a hash alone says nothing to a human.
/// Capped because a header is caller-controlled and this one is stored.
fn user_agent(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get("user-agent")?.to_str().ok()?.trim();
    if raw.is_empty() {
        return None;
    }
    Some(raw.chars().take(160).collect())
}

/// Pull the presented key out of either header form. The MCP endpoint
/// authenticates the same way, which is why this is crate-visible.
pub(crate) fn presented_key(headers: &HeaderMap) -> Option<String> {
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
    let Some((client_name, client)) = state.authenticate(&config, &presented) else {
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
    //
    //    Why a route was skipped is carried rather than dropped. A skip that a
    //    fallback then covers is a routing decision and worth nothing, but a
    //    request where *every* route was skipped ends in a 403 the client
    //    cannot act on and that used to leave no trace anywhere: no `calls`
    //    row, since nothing was forwarded, and no alert, since the scrubber
    //    never ran. The broker refusing to spend money is exactly the kind of
    //    thing an audit log exists for.
    let mut skipped: Vec<(&str, &'static str, String)> = Vec::new();
    let mut last: Option<Response> = None;
    for route_name in chain {
        let route = &config.routes[route_name];
        let provider = &config.providers[&route.provider];

        if !client.routes.is_empty() && !client.routes.iter().any(|r| r == route_name) {
            skipped.push((
                route_name,
                "route",
                format!("`{client_name}` is not allowed on this route"),
            ));
            continue;
        }
        if let Some(cap) = client.max_trust
            && provider.trust > cap
        {
            skipped.push((
                route_name,
                "trust",
                format!(
                    "`{}` is trusted `{}`, above `{client_name}`'s cap of `{}`",
                    route.provider,
                    trust_label(provider.trust),
                    trust_label(cap)
                ),
            ));
            continue;
        }

        // 4. Look at what is about to leave, for this destination's trust level.
        let scrubber = state.scrubber();
        let (hits, verdict) = scrubber.inspect(&body, provider.trust);
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
                    Action::Substitute => "substitute",
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

        // The same scrubber snapshot that produced the verdict does the
        // rewriting, so a vault write between the two cannot leave a value the
        // verdict never saw. On any other verdict the original bytes travel.
        let outgoing: std::borrow::Cow<'_, [u8]> = if verdict == Action::Substitute {
            match scrubber.substitute(&body) {
                Some(rewritten) => std::borrow::Cow::Owned(rewritten),
                None => std::borrow::Cow::Borrowed(&body),
            }
        } else {
            std::borrow::Cow::Borrowed(&body)
        };

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
                bytes_up: outgoing.len(),
                user_agent: user_agent(&headers),
            })
            .unwrap_or(-1);

        match forward(
            &state,
            provider,
            &outgoing,
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
        // Nothing was forwarded and nothing failed upstream, so every route in
        // the chain was skipped. One alert per skipped route, because "which
        // cap stopped this" is the whole of the answer and a single line saying
        // "refused" would send the reader back to the config to guess.
        for (route, kind, _) in &skipped {
            let _ = state.audit.alert(&NewAlert {
                call_id: None,
                client: client_name,
                provider: &config.routes[*route].provider,
                trust: trust_label(config.providers[&config.routes[*route].provider].trust),
                kind,
                label: route,
                action: "block",
            });
            tracing::warn!(
                client = client_name,
                route = route,
                reason = kind,
                "refused to route, nothing was sent"
            );
        }
        let why = skipped
            .iter()
            .map(|(route, _, why)| format!("`{route}`: {why}"))
            .collect::<Vec<_>>()
            .join("; ");
        json_error(
            StatusCode::FORBIDDEN,
            "permission_error",
            &format!(
                "no route this client is allowed to use can serve `{}`. {why}",
                peeked.model
            ),
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

    // The one place the body stops being forwarded byte for byte. A provider
    // that only sells the OpenAI shape gets the request rewritten here and its
    // answer rewritten back below, so everything either side of `forward` still
    // sees the Anthropic shape and only the Anthropic shape.
    let outgoing = match provider.wire {
        Wire::Anthropic => outgoing,
        Wire::Openai => match wire::request_to_openai(&outgoing) {
            Ok(v) => v,
            // A 400 rather than a 500: the request is the thing that cannot be
            // carried, and the message says which part.
            Err(e) => {
                return Ok((
                    StatusCode::BAD_REQUEST,
                    json_error(StatusCode::BAD_REQUEST, "invalid_request_error", &e.0),
                ));
            }
        },
    };

    let url = format!(
        "{}{}",
        provider.base_url.trim_end_matches('/'),
        provider.wire.path()
    );
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
            .secret(key_ref)
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

    // Translated before the tap, never after. The tap, the pricing, the audit
    // row and the dashboard were all written against the Anthropic shape, and
    // they stay that way because nothing downstream of this line ever sees the
    // other one.
    let translated = wire::translate_stream(
        upstream.bytes_stream(),
        provider.wire,
        sse,
        model_sent.to_owned(),
    );

    // Streamed straight through, with a tap on the way past. Buffering would
    // break SSE, and buffering a long generation would also mean holding the
    // whole answer in memory for no reason.
    let tapped = tap_stream(
        translated,
        Ledger {
            state: state.clone(),
            call_id,
            started,
            status: status.as_u16(),
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
    tap: Tap,
}

impl Drop for Ledger {
    fn drop(&mut self) {
        self.tap.finish();
        let _ = self.state.audit.finish(
            self.call_id,
            &Finish {
                status: self.status,
                duration_ms: self.started.elapsed().as_millis() as u64,
                bytes_down: self.tap.bytes,
                usage: self.tap.usage(),
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

/// What this caller may ask for, in the shape both dialects publish it.
///
/// The point is to stop every client carrying its own copy of the routing
/// table. A launcher used to hold a hand-written list that had to be edited in
/// step with `douane.toml`, and forgetting either produced `no route for model`
/// from opposite causes.
///
/// **The answer is per client, not per instance.** The caps are the same ones
/// the relay enforces on a real call: a client restricted to a few routes sees
/// those routes, and a client capped at a trust level never learns that the
/// resellers exist. Anything else would turn this into a map of every upstream
/// the broker knows, handed out on any valid virtual key.
///
/// Nothing here names a credential, a base URL or a provider. The route name is
/// the client's whole vocabulary, which is the same reason the relay answers on
/// one.
///
/// `?provider=a,b` narrows the answer to those upstreams, for a launcher whose
/// menu has one entry per subscription and needs to know which names belong to
/// which. It is a filter and never a disclosure: the caller has to already know
/// the name to type it, the answer still carries no provider, and it applies
/// after the client's own caps, so it can only ever return less.
#[derive(Debug, serde::Deserialize)]
pub struct ModelsQuery {
    #[serde(default)]
    provider: Option<String>,
}

pub async fn models(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<ModelsQuery>,
) -> Response {
    let config = state.config();

    let Some(presented) = presented_key(&headers) else {
        return json_error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "no virtual key presented",
        );
    };
    let Some((_, client)) = state.authenticate(&config, &presented) else {
        return json_error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "unknown virtual key",
        );
    };

    // Labels and windows for the routes that came from a catalogue. A route
    // written by hand in the TOML has neither, and answering without them is
    // right: the file says where a name goes, not what the model is called.
    let described: std::collections::HashMap<String, crate::catalog::ModelEntry> = state
        .audit
        .models()
        .unwrap_or_default()
        .into_iter()
        .map(|m| (m.route.clone(), m))
        .collect();

    let wanted: Vec<&str> = q
        .provider
        .as_deref()
        .map(|s| s.split(',').map(str::trim).filter(|p| !p.is_empty()).collect())
        .unwrap_or_default();

    let mut data = Vec::new();
    for (name, route) in &config.routes {
        if !client.routes.is_empty() && !client.routes.iter().any(|r| r == name) {
            continue;
        }
        let Some(provider) = config.providers.get(&route.provider) else {
            continue;
        };
        if let Some(cap) = client.max_trust
            && provider.trust > cap
        {
            continue;
        }
        if !wanted.is_empty() && !wanted.iter().any(|p| *p == route.provider) {
            continue;
        }
        let known = described.get(name);
        // The file first, the catalogue second. A route written by hand is the
        // curated one, and what its author called it outranks what a reseller
        // calls the model underneath.
        let mut entry = serde_json::json!({
            "type": "model",
            "id": name,
            "display_name": route
                .label
                .clone()
                .or_else(|| known.and_then(|m| m.label.clone()))
                .unwrap_or_else(|| name.clone()),
        });
        // An extension both dialects tolerate, and the reason a launcher can
        // stop keeping its own table of context windows.
        if let Some(n) = route.context_length.or(known.and_then(|m| m.context_length)) {
            entry["context_length"] = serde_json::json!(n);
        }
        data.push(entry);
    }

    axum::Json(serde_json::json!({
        "data": data,
        "has_more": false,
    }))
    .into_response()
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
