//! Subscription routing, OAuth, quota accounting, and native streaming transports.
pub mod affinity;
pub mod ledger;
pub mod management;
pub mod quota_view;
pub mod relay;
pub mod scheduler;
pub mod settings;
pub mod socket;
mod socket_compat;
pub mod state;
pub mod subscriptions;
pub mod translate;
pub mod wire;

use axum::{
    Router, middleware,
    routing::{get, patch, post},
};
use state::AppState;
use std::sync::Arc;

pub fn router(state: Arc<AppState>) -> Router {
    let management = Router::new()
        .route("/accounts", get(management::accounts))
        .route("/accounts/import", post(management::import))
        .route(
            "/accounts/{id}/verification",
            get(management::account_verification),
        )
        .route(
            "/accounts/{id}",
            patch(management::update_account).delete(management::delete_account),
        )
        .route("/quotas", get(management::quotas))
        .route("/accounts/{id}/resets", get(management::account_resets))
        .route(
            "/accounts/{id}/resets/redeem",
            post(management::redeem_reset),
        )
        .route("/quotas/refresh", post(management::refresh_quotas))
        .route("/models", get(management::models))
        .route("/models/refresh", post(management::refresh_models))
        .route("/models/{*id}", patch(management::set_model))
        .route("/calls", get(management::calls))
        .route("/summary", get(management::summary))
        .route("/status", get(management::summary))
        .route(
            "/settings",
            get(management::settings)
                .patch(management::update_settings)
                .put(management::update_settings),
        )
        .route("/providers", get(management::providers))
        .route("/oauth/start", post(management::oauth_start))
        .route("/oauth/callback", post(management::oauth_callback))
        .route("/oauth/{id}", get(management::oauth_status))
        .route(
            "/oauth/{id}/verification",
            get(management::oauth_verification),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            management::guard,
        ));
    let compatible = Router::new()
        .route(
            "/auth-files",
            get(management::auth_files)
                .post(management::import)
                .delete(management::delete_auth_file),
        )
        .route("/auth-files/status", patch(management::patch_auth_file))
        .route("/auth-files/models", get(management::auth_file_models))
        .route("/config", get(management::compatible_config))
        .route("/usage", get(management::summary))
        .route("/logs", get(management::calls))
        .route("/models", get(management::public_models))
        .route("/latest-version", get(management::version))
        .route("/version", get(management::version))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            management::guard,
        ));
    Router::new()
        .route(
            "/",
            get(|| async { axum::response::Redirect::temporary("/admin") }),
        )
        .route("/health", get(management::health))
        .route("/v1/models", get(management::public_models))
        .route("/v1/quotas", get(management::public_quotas))
        .route("/v1/messages", post(relay::forward))
        .route("/v1/messages/count_tokens", post(relay::forward))
        .route("/v1/chat/completions", post(relay::forward))
        .route("/v1/responses", post(relay::forward).get(socket::upgrade))
        .route("/v1/responses/compact", post(relay::forward))
        .route("/admin", get(management::page))
        .route("/admin/", get(management::page))
        .route(
            "/management.html",
            get(|| async { axum::response::Redirect::temporary("/admin") }),
        )
        .route("/admin/_app/{*path}", get(management::asset))
        .nest("/admin/api", management)
        .nest("/v0/management", compatible.clone())
        .nest("/v8/management", compatible)
        .layer(tower_http::limit::RequestBodyLimitLayer::new(
            64 * 1024 * 1024,
        ))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}
