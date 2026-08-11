//! The dashboard: what went where, where it goes next, and what is in the safe.
//!
//! Three jobs. It answers "which agent spent what, talking to whom", which is
//! the question the request log exists for. It repoints a route while the
//! broker runs, which is what makes routing by model name useful rather than
//! merely indirect: an agent asks for `haiku` for a whole session, and what
//! answers can change between two turns without that session knowing. And it
//! deposits a credential, so storing one no longer means an ssh session and a
//! CLI on the machine that holds the vault.
//!
//! **Deposits only.** Every read surface here returns [`douane_vault::EntryInfo`],
//! which has no field a value could be put in, and there is no endpoint that
//! hands one back. A dashboard that could show a secret would be a nicer way to
//! steal one than the thing this project was built to stop.
//!
//! Everything is served from the binary: the page is a compiled single-page app
//! embedded at build time, so there is no second process to keep alive and
//! nothing is fetched from a CDN at page load. A page about credentials has no
//! business making third-party requests, fonts included.
//!
//! The token guards `/admin/api/*` and not the page itself. The page is a
//! static bundle and reveals nothing; it asks for the token and sends it as a
//! header. A token in the URL would land in the access log of everything it
//! passed through, which is the same mistake this project exists to avoid.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use douane_vault::{Entry, Secret, SecretRef};
use include_dir::{Dir, include_dir};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::config::RouteOverride;
use crate::proxy::{AppState, json_error};

/// The compiled dashboard, as built by `pnpm build` in `dash/`.
///
/// Committed rather than built by `build.rs`: the machine that compiles this
/// binary has a Rust toolchain and no node, and making a broker's build depend
/// on npm resolving would be a strange thing to do to a security tool.
static ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../dash/build");

const INDEX: &str = "index.html";

fn mime_for(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// What the page is allowed to talk to: itself, and nothing else.
///
/// This is the guarantee, not the bundle's contents. Auditing a minified bundle
/// for addresses does not work, because a framework legitimately carries the
/// XHTML namespace and links to its own documentation in warning messages, and
/// none of those are requests. `connect-src` and `font-src` are what actually
/// stop a dependency from fetching or exfiltrating anything, whatever strings it
/// happens to hold.
///
/// `unsafe-inline` for scripts is the price of SvelteKit's bootstrap, which is
/// inlined into the shell and whose hash changes with every build. It costs
/// nothing against the threat being defended here: no origin but this one can be
/// reached, so injected script has nowhere to send what it reads.
const CSP: &str = "default-src 'none'; \
     script-src 'self' 'unsafe-inline'; \
     style-src 'self' 'unsafe-inline'; \
     img-src 'self' data:; \
     font-src 'self'; \
     connect-src 'self'; \
     base-uri 'none'; \
     form-action 'none'; \
     frame-ancestors 'none'";

fn serve(path: &str) -> Response {
    let Some(file) = ASSETS.get_file(path) else {
        return json_error(
            StatusCode::NOT_FOUND,
            "not_found_error",
            "no such dashboard asset",
        );
    };
    // Everything under `immutable` carries a content hash in its name, so it can
    // be cached forever and a new build simply asks for a different name. The
    // page itself must not be, or a deploy would leave a browser pointing at
    // assets that no longer exist.
    let cache = if path.contains("immutable/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        [
            (header::CONTENT_TYPE, mime_for(path)),
            (header::CACHE_CONTROL, cache),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        file.contents(),
    )
        .into_response()
}

pub async fn page() -> Response {
    let mut response = serve(INDEX);
    let h = response.headers_mut();
    h.insert(header::CONTENT_SECURITY_POLICY, CSP.parse().unwrap());
    // The dashboard is reached through a hostname that says what it is. Nothing
    // downstream of a click needs to be told where the click came from.
    h.insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    response
}

pub async fn asset(Path(path): Path<String>) -> Response {
    // `include_dir` resolves through the embedded tree and has no notion of a
    // parent directory, so `..` finds nothing rather than escaping. Rejecting it
    // anyway keeps the intent readable to whoever changes the backing store.
    if path.contains("..") {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "no such dashboard asset",
        );
    }
    serve(&format!("_app/{path}"))
}

/// The response to send back, or `None` when the caller may proceed.
///
/// Compared in constant time through `Secret::ct_eq`, the same path the virtual
/// keys take: a token guarding the routing table deserves no less care than one
/// guarding a model call.
fn denied(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    let Some(expected) = state.admin_token() else {
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
    fn hours(&self) -> i64 {
        self.hours.unwrap_or(24).clamp(1, 24 * 365)
    }

    fn since(&self) -> String {
        (OffsetDateTime::now_utc() - time::Duration::hours(self.hours()))
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
    match state.audit.summary(&w.since(), w.hours()) {
        Ok(s) => Json(serde_json::json!({ "summary": s })).into_response(),
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
    Json(serde_json::json!({
        "entries": state.vault_list(),
        "in_use": in_use(&state),
        "watching": state.scrubber().watching(),
    }))
    .into_response()
}

/// Every reference the running configuration depends on.
///
/// Deleting one of these would break the broker on its next request, and
/// replacing one is a deliberate act rather than a slip, so the dashboard marks
/// them and the delete handler refuses them outright.
fn in_use(state: &AppState) -> Vec<String> {
    let live = state.config();
    let mut refs: Vec<String> = live
        .providers
        .values()
        .filter_map(|p| p.key.as_ref())
        .chain(live.clients.values().map(|c| &c.key))
        .chain(state.base.server.admin_key.iter())
        .map(|r| r.to_string())
        .collect();
    refs.sort();
    refs.dedup();
    refs
}

/// What the dashboard may send about a secret.
///
/// `value` absent means "leave the value alone and edit the note", which is the
/// only way to annotate an entry without the browser having to hold a
/// credential it does not need.
#[derive(Debug, Deserialize)]
pub struct Deposit {
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

/// A credential is a line of text. Anything larger is a mistake on the way in,
/// and storing it would only make the vault file and every scrubber pass bigger.
const MAX_SECRET_BYTES: usize = 8 * 1024;

fn vault_error(e: douane_vault::VaultError) -> Response {
    tracing::error!("vault write failed: {e}");
    json_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "api_error",
        "the vault could not be written",
    )
}

/// Store or replace a credential, or rewrite its note.
///
/// The value arrives in the body and never in the path: a URL is written to
/// every access log between the browser and here, which is the one place a
/// secret must never be.
pub async fn put_secret(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((namespace, key)): Path<(String, String)>,
    Json(d): Json<Deposit>,
) -> Response {
    if let Some(r) = denied(&state, &headers) {
        return r;
    }
    // A namespace holding a dot could never be read back: `SecretRef::parse`
    // splits on the first one, so `a.b.c` would resolve to namespace `a`.
    if namespace.is_empty() || key.is_empty() || namespace.contains('.') {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "a reference is `namespace.key`, and the namespace may not contain a dot",
        );
    }
    let r = SecretRef::new(&namespace, &key);

    let Some(value) = d.value else {
        // Note only. An entry that does not exist yet has no note to change.
        return match state.vault_renote(&r, d.note) {
            Ok(true) => stored(&state, &r),
            Ok(false) => json_error(
                StatusCode::NOT_FOUND,
                "not_found_error",
                &format!("no secret at `{r}` to annotate"),
            ),
            Err(e) => vault_error(e),
        };
    };

    if value.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "an empty value is a deletion, not a deposit",
        );
    }
    if value.len() > MAX_SECRET_BYTES {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "that is too large to be a credential",
        );
    }

    let mut entry = Entry::new(Secret::new(value));
    entry.note = d.note;
    match state.vault_put(&r, entry) {
        // The reference alone, never the value, and never the fingerprint of
        // what was just sent: the log of a broker is not a place to leave a
        // trail back to a credential.
        Ok(()) => {
            tracing::info!(secret = %r, "a credential was deposited through the dashboard");
            stored(&state, &r)
        }
        Err(e) => vault_error(e),
    }
}

pub async fn delete_secret(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((namespace, key)): Path<(String, String)>,
) -> Response {
    if let Some(r) = denied(&state, &headers) {
        return r;
    }
    let r = SecretRef::new(&namespace, &key);
    if in_use(&state).contains(&r.to_string()) {
        return json_error(
            StatusCode::CONFLICT,
            "invalid_request_error",
            &format!("`{r}` is what the running configuration authenticates with"),
        );
    }
    match state.vault_delete(&r) {
        Ok(true) => {
            tracing::info!(secret = %r, "a credential was removed through the dashboard");
            Json(serde_json::json!({
                "deleted": r.to_string(),
                "watching": state.scrubber().watching(),
            }))
            .into_response()
        }
        Ok(false) => json_error(
            StatusCode::NOT_FOUND,
            "not_found_error",
            &format!("no secret at `{r}`"),
        ),
        Err(e) => vault_error(e),
    }
}

/// What a write answers with: the entry as a listing sees it, plus how many
/// values the scrubber now holds, so the page can say the deposit is being
/// watched rather than merely stored.
fn stored(state: &AppState, r: &SecretRef) -> Response {
    let info = state
        .vault_list()
        .into_iter()
        .find(|e| e.namespace == r.namespace && e.key == r.key);
    Json(serde_json::json!({
        "entry": info,
        "watching": state.scrubber().watching(),
    }))
    .into_response()
}

#[derive(Debug, Serialize)]
struct ProviderView {
    name: String,
    base_url: String,
    trust: &'static str,
    /// Whether a credential is configured. Never which one, never its value.
    keyed: bool,
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
        "scrub": state.base.scrub,
        "watching": state.scrubber().watching(),
        "egress": {
            "listening": state.base.egress.listen.is_some(),
            "rules": state.base.egress.rules.len(),
        },
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
    fn the_compiled_dashboard_is_actually_embedded() {
        // A `dash/build` that was never built embeds as an empty directory and
        // the binary then serves a 404 where the dashboard should be, which
        // looks at deploy time exactly like a routing mistake.
        assert!(
            ASSETS.get_file(INDEX).is_some(),
            "no {INDEX} in the embedded bundle: run `pnpm build` in dash/"
        );
        assert!(
            ASSETS.get_dir("_app/immutable").is_some(),
            "the embedded bundle carries no hashed assets"
        );
    }

    #[test]
    fn the_markup_and_the_styles_fetch_nothing_off_origin() {
        // The two files that can pull in a third party without any script
        // running: a `<link>` or `<script src>` in the shell, and a `url()` or
        // `@import` in the stylesheet. Fonts are the usual way this slips back
        // in, which is why they are bundled rather than linked.
        //
        // The compiled JavaScript is deliberately not audited this way. A
        // framework carries the XHTML namespace and links to its own docs inside
        // warning strings, so a substring search there reports addresses that
        // are never requested. What actually holds for the bundle is the policy
        // in `CSP`, checked below.
        let index = ASSETS.get_file(INDEX).unwrap().contents_utf8().unwrap();
        let mut styles = 0;
        for file in ASSETS
            .get_dir("_app/immutable/assets")
            .into_iter()
            .flat_map(|d| d.files())
            .filter(|f| f.path().extension().is_some_and(|e| e == "css"))
        {
            let css = file.contents_utf8().unwrap();
            styles += 1;
            for needle in ["url(http", "url('http", "url(\"http", "@import \"http"] {
                assert!(
                    !css.contains(needle),
                    "{} fetches {needle}",
                    file.path().display()
                );
            }
        }
        assert!(
            styles > 0,
            "no stylesheet was checked, so nothing was proved"
        );

        for needle in ["http://", "https://", "//cdn", "integrity="] {
            assert!(
                !index.contains(needle),
                "the page shell references {needle}"
            );
        }
    }

    #[tokio::test]
    async fn the_page_forbids_reaching_any_other_origin() {
        // The bundle is allowed to hold whatever strings its dependencies ship
        // with, because this is what decides whether a request can leave.
        let response = page().await;
        let csp = response
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .expect("the dashboard shell must carry a content security policy")
            .to_str()
            .unwrap();
        for directive in [
            "default-src 'none'",
            "connect-src 'self'",
            "font-src 'self'",
            "frame-ancestors 'none'",
        ] {
            assert!(csp.contains(directive), "the policy is missing {directive}");
        }
    }

    #[test]
    fn assets_are_typed() {
        assert_eq!(mime_for("app.js"), "text/javascript; charset=utf-8");
        assert_eq!(mime_for("geist.woff2"), "font/woff2");
        assert_eq!(mime_for("no-extension"), "application/octet-stream");
    }
}
