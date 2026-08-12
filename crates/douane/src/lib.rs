//! The douane broker, as a library.
//!
//! The binary is a thin wrapper: read the config, open the vault, build the
//! state, serve [`router`]. Everything worth testing lives here, so a test can
//! stand a real broker up against a real upstream over a real socket rather
//! than assert on the pieces separately and hope they fit together.

pub mod audit;
pub mod catalog;
pub mod config;
pub mod dash;
pub mod egress;
pub mod guard;
pub mod mcp;
pub mod proxy;
pub mod scrub;
pub mod usage;
pub mod wire;

use std::sync::Arc;

use axum::Router;
use axum::routing::{delete, get, post, put};

use crate::proxy::AppState;

/// Every route douane serves.
///
/// The 64 MiB body limit is generous on purpose: a long conversation with
/// images in it is legitimately large, and a request refused here looks to the
/// agent exactly like a model failure.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/messages", post(proxy::messages))
        // What this caller may ask for. Authenticated by the same virtual keys
        // as the relay and filtered by the same caps, so a client never learns
        // that a route it cannot reach exists. This is what lets a launcher
        // stop keeping a hand-written copy of the routing table.
        .route("/v1/models", get(proxy::models))
        .route("/health", get(proxy::health))
        // Stateless MCP: one POST per request, no session, Tasks for anything
        // that waits on a human. Guarded by the same virtual keys as the relay.
        .route("/mcp", post(mcp::endpoint))
        // The redemption path: the one surface that answers with a value,
        // gated on a human approval. See `mcp::redeem` for why it exists.
        .route("/agent/api/use", post(mcp::redeem))
        .route("/admin", get(dash::page))
        .route("/admin/", get(dash::page))
        // The compiled bundle. Every asset SvelteKit emits lives under `_app`,
        // so one wildcard covers them and nothing else is reachable: a static
        // file server rooted at the whole bundle would be a wider door than
        // this page needs.
        .route("/admin/_app/{*path}", get(dash::asset))
        .route("/admin/api/summary", get(dash::summary))
        .route("/admin/api/calls", get(dash::calls))
        .route("/admin/api/threads", get(dash::threads))
        .route("/admin/api/alerts", get(dash::alerts))
        .route("/admin/api/vault", get(dash::vault))
        .route("/admin/api/vault/{namespace}/{key}", put(dash::put_secret))
        .route(
            "/admin/api/vault/{namespace}/{key}",
            delete(dash::delete_secret),
        )
        .route("/admin/api/config", get(dash::config))
        // POST on purpose: a probe sends one real, paid call per provider.
        .route("/admin/api/providers/probe", post(dash::probe))
        .route("/admin/api/routes/{name}", put(dash::set_route))
        .route("/admin/api/routes/{name}", delete(dash::clear_route))
        // The catalogue. Reading it is free; refreshing it spends one request
        // per upstream, so that one is a POST like the probe is.
        .route("/admin/api/models", get(dash::models))
        .route("/admin/api/models", put(dash::put_model))
        .route("/admin/api/models/refresh", post(dash::refresh_models))
        .route(
            "/admin/api/models/{route}/enabled",
            put(dash::set_model_enabled),
        )
        .route("/admin/api/models/{route}", delete(dash::delete_model))
        .route(
            "/admin/api/providers/{provider}/models/enabled",
            put(dash::set_provider_models_enabled),
        )
        .route("/admin/api/grants", get(dash::grants))
        .route("/admin/api/grants/{id}/decide", post(dash::decide_grant))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(
            64 * 1024 * 1024,
        ))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}
