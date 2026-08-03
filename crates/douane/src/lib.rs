//! The douane broker, as a library.
//!
//! The binary is a thin wrapper: read the config, open the vault, build the
//! state, serve [`router`]. Everything worth testing lives here, so a test can
//! stand a real broker up against a real upstream over a real socket rather
//! than assert on the pieces separately and hope they fit together.

pub mod audit;
pub mod config;
pub mod dash;
pub mod egress;
pub mod guard;
pub mod proxy;
pub mod scrub;
pub mod usage;

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
        .route("/health", get(proxy::health))
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
        .route("/admin/api/routes/{name}", put(dash::set_route))
        .route("/admin/api/routes/{name}", delete(dash::clear_route))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(
            64 * 1024 * 1024,
        ))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}
