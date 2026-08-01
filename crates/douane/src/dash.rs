//! The dashboard: what went where, and where it goes next.
//!
//! Two jobs. It answers "which agent spent what, talking to whom", which is the
//! question the request log exists for, and it repoints a route while the
//! broker runs, which is what makes routing by model name useful rather than
//! merely indirect. An agent asks for `haiku` for a whole session; what answers
//! can change between two turns without that session knowing.
//!
//! Everything is served from the binary. No build step, no npm, no second
//! process to keep alive, and nothing fetched from a CDN at page load: a page
//! about credentials has no business making third-party requests.
//!
//! The token guards `/admin/api/*` and not the page itself. The page is static
//! markup and reveals nothing; it asks for the token and sends it as a header.
//! A token in the URL would land in the access log of everything it passed
//! through, which is the same mistake this project exists to avoid.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::config::RouteOverride;
use crate::proxy::{AppState, json_error};

const PAGE: &str = include_str!("dash.html");

pub async fn page() -> Html<&'static str> {
    Html(PAGE)
}

/// The response to send back, or `None` when the caller may proceed.
///
/// Compared in constant time through `Secret::ct_eq`, the same path the virtual
/// keys take: a token guarding the routing table deserves no less care than one
/// guarding a model call.
fn denied(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    let Some(expected) = &state.admin_token else {
        return Some(json_error(
            StatusCode::NOT_FOUND,
            "not_found_error",
            "no dashboard on this instance: set `server.admin_key` and store it",
        ));
    };
    let presented = headers
        .get("x-douane-admin")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if expected.ct_eq(presented) {
        None
    } else {
        Some(json_error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "bad or missing admin token",
        ))
    }
}

/// `?hours=24`. A window in hours rather than a timestamp, so nothing a browser
/// sends ever reaches a query as text.
#[derive(Debug, Deserialize)]
pub struct Window {
    #[serde(default)]
    hours: Option<i64>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    client: Option<String>,
    #[serde(default)]
    thread: Option<String>,
}

impl Window {
    fn since(&self) -> String {
        let hours = self.hours.unwrap_or(24).clamp(1, 24 * 365);
        (OffsetDateTime::now_utc() - time::Duration::hours(hours))
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default()
    }

    fn limit(&self) -> u32 {
        self.limit.unwrap_or(100).clamp(1, 2000)
    }
}

fn db_error(e: rusqlite::Error) -> Response {
    tracing::error!("dashboard query failed: {e}");
    json_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "api_error",
        "the request log could not be read",
    )
}

pub async fn summary(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(w): Query<Window>,
) -> Response {
    if let Some(r) = denied(&state, &headers) {
        return r;
    }
    match state.audit.summary(&w.since()) {
        Ok(s) => Json(serde_json::json!({
            "currency": state.base.server.currency,
            "summary": s,
        }))
        .into_response(),
        Err(e) => db_error(e),
    }
}

pub async fn calls(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(w): Query<Window>,
) -> Response {
    if let Some(r) = denied(&state, &headers) {
        return r;
    }
    match state
        .audit
        .recent(w.limit(), w.client.as_deref(), w.thread.as_deref())
    {
        Ok(c) => Json(c).into_response(),
        Err(e) => db_error(e),
    }
}

pub async fn threads(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(w): Query<Window>,
) -> Response {
    if let Some(r) = denied(&state, &headers) {
        return r;
    }
    match state.audit.threads(&w.since(), w.limit()) {
        Ok(t) => Json(t).into_response(),
        Err(e) => db_error(e),
    }
}

pub async fn alerts(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(w): Query<Window>,
) -> Response {
    if let Some(r) = denied(&state, &headers) {
        return r;
    }
    match state.audit.alerts(w.limit()) {
        Ok(a) => Json(a).into_response(),
        Err(e) => db_error(e),
    }
}

/// The vault, as names. No value can travel this route: `EntryInfo` has no
/// field to put one in.
pub async fn vault(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(r) = denied(&state, &headers) {
        return r;
    }
    Json(state.vault.list()).into_response()
}

#[derive(Debug, Serialize)]
struct ProviderView {
    name: String,
    base_url: String,
    trust: &'static str,
    /// Whether a credential is configured. Never which one, never its value.
    keyed: bool,
    priced: bool,
}

#[derive(Debug, Serialize)]
struct RouteView {
    name: String,
    provider: String,
    model: Option<String>,
    fallback: Vec<String>,
    /// What the TOML says, when an override is currently pointing this route
    /// somewhere else. The dashboard shows both so nobody has to open the file
    /// to find out what "revert" would do.
    base_provider: String,
    base_model: Option<String>,
    overridden: bool,
}

#[derive(Debug, Serialize)]
struct ClientView {
    name: String,
    routes: Vec<String>,
    max_trust: Option<&'static str>,
    note: Option<String>,
}

fn trust_label(t: crate::config::Trust) -> &'static str {
    match t {
        crate::config::Trust::Max => "max",
        crate::config::Trust::Mid => "mid",
        crate::config::Trust::Low => "low",
    }
}

pub async fn config(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(r) = denied(&state, &headers) {
        return r;
    }
    let live = state.config();

    let providers: Vec<_> = live
        .providers
        .iter()
        .map(|(name, p)| ProviderView {
            name: name.clone(),
            base_url: p.base_url.clone(),
            trust: trust_label(p.trust),
            keyed: p.key.is_some(),
            priced: p.pricing.is_some(),
        })
        .collect();

    let routes: Vec<_> = live
        .routes
        .iter()
        .map(|(name, r)| {
            let base = &state.base.routes[name];
            RouteView {
                name: name.clone(),
                provider: r.provider.clone(),
                model: r.model.clone(),
                fallback: r.fallback.clone(),
                base_provider: base.provider.clone(),
                base_model: base.model.clone(),
                overridden: base.provider != r.provider || base.model != r.model,
            }
        })
        .collect();

    let clients: Vec<_> = live
        .clients
        .iter()
        .map(|(name, c)| ClientView {
            name: name.clone(),
            routes: c.routes.clone(),
            max_trust: c.max_trust.map(trust_label),
            note: c.note.clone(),
        })
        .collect();

    Json(serde_json::json!({
        "providers": providers,
        "routes": routes,
        "clients": clients,
        "currency": state.base.server.currency,
        "scrub": state.base.scrub,
        "watching": state.scrubber.watching(),
    }))
    .into_response()
}

/// Repoint a route. The change lands in the database and takes effect on the
/// next request; the TOML is never rewritten, so a restart reverts to the file.
pub async fn set_route(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(o): Json<RouteOverride>,
) -> Response {
    if let Some(r) = denied(&state, &headers) {
        return r;
    }
    if !state.base.routes.contains_key(&name) {
        return json_error(
            StatusCode::NOT_FOUND,
            "not_found_error",
            &format!("no route named `{name}`"),
        );
    }
    if let Some(p) = &o.provider
        && !state.base.providers.contains_key(p)
    {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            &format!("no provider named `{p}`"),
        );
    }
    if let Err(e) = state.audit.set_override(&name, &o) {
        return db_error(e);
    }
    apply(&state, &name)
}

pub async fn clear_route(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Some(r) = denied(&state, &headers) {
        return r;
    }
    match state.audit.clear_override(&name) {
        Ok(_) => apply(&state, &name),
        Err(e) => db_error(e),
    }
}

fn apply(state: &Arc<AppState>, name: &str) -> Response {
    match state.refresh_routes() {
        Ok(()) => {
            let live = state.config();
            let r = &live.routes[name];
            tracing::info!(route = name, provider = r.provider, "routing changed");
            Json(serde_json::json!({
                "route": name,
                "provider": r.provider,
                "model": r.model,
            }))
            .into_response()
        }
        Err(e) => {
            tracing::error!("could not apply the routing change: {e}");
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "api_error",
                "the change was stored but could not be applied",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_defaults_to_a_day_and_refuses_silly_values() {
        let now = OffsetDateTime::now_utc();
        let parse = |s: &str| {
            OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).unwrap()
        };

        let w = Window {
            hours: None,
            limit: None,
            client: None,
            thread: None,
        };
        let delta = now - parse(&w.since());
        assert!((delta.whole_hours() - 24).abs() <= 1);
        assert_eq!(w.limit(), 100);

        let w = Window {
            hours: Some(-500),
            limit: Some(0),
            client: None,
            thread: None,
        };
        // A negative window would read as "the future" and return nothing at
        // all, which looks identical to "no traffic".
        assert!(parse(&w.since()) < now);
        assert_eq!(w.limit(), 1);

        let w = Window {
            hours: Some(1_000_000),
            limit: Some(99_999),
            client: None,
            thread: None,
        };
        assert_eq!(w.limit(), 2000);
    }

    #[test]
    fn the_page_carries_no_third_party_reference() {
        // A page about credentials that phones home to a CDN would hand a third
        // party the timing of every visit, and a compromised CDN the contents.
        for needle in ["http://", "https://", "//cdn", "integrity="] {
            assert!(
                !PAGE.contains(needle),
                "the dashboard page must not reference {needle}"
            );
        }
    }
}
