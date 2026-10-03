use crate::{relay::error, state::AppState, subscriptions::SubscriptionProvider};
use axum::{
    Json,
    extract::{Path, Query, Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::{Arc, atomic::Ordering},
};

static ASSETS: include_dir::Dir<'_> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/../../dash/build");
fn asset_response(path: &str) -> Response {
    let Some(file) = ASSETS.get_file(path) else {
        return error(StatusCode::NOT_FOUND, "asset not found");
    };
    let mime = if path.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if path.ends_with(".js") {
        "application/javascript; charset=utf-8"
    } else if path.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else if path.ends_with(".woff2") {
        "font/woff2"
    } else if path.ends_with(".json") {
        "application/json"
    } else {
        "application/octet-stream"
    };
    (
        [
            (header::CONTENT_TYPE, mime),
            (
                header::CACHE_CONTROL,
                if path.contains("immutable/") {
                    "public, max-age=31536000, immutable"
                } else {
                    "no-cache"
                },
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        file.contents(),
    )
        .into_response()
}
pub async fn page() -> Response {
    asset_response(if ASSETS.get_file("index.html").is_some() {
        "index.html"
    } else {
        "200.html"
    })
}
pub async fn asset(Path(path): Path<String>) -> Response {
    asset_response(&format!("_app/{path}"))
}
pub async fn health() -> Json<Value> {
    Json(
        json!({"status":"ok","version":env!("CARGO_PKG_VERSION"),"service":"douane","websocket":true}),
    )
}

pub async fn guard(State(state): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    if let Some(expected) = &state.management_token {
        let presented = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .or_else(|| {
                request
                    .headers()
                    .get("x-management-key")
                    .and_then(|v| v.to_str().ok())
            })
            .or_else(|| {
                request
                    .headers()
                    .get("x-douane-admin")
                    .and_then(|v| v.to_str().ok())
            });
        if presented != Some(expected.as_str()) {
            return error(
                StatusCode::UNAUTHORIZED,
                "management authentication required",
            );
        }
    }
    if request.method() != axum::http::Method::GET {
        if let Some(origin) = request
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
        {
            let host = request
                .headers()
                .get(header::HOST)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let matches = reqwest::Url::parse(origin)
                .ok()
                .is_some_and(|u| u.authority() == host);
            if !matches {
                return error(
                    StatusCode::FORBIDDEN,
                    "management writes require the same origin",
                );
            }
        }
    }
    next.run(request).await
}

pub async fn accounts(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({"accounts":state.accounts.list().await}))
}
pub async fn import(State(state): State<Arc<AppState>>, Json(body): Json<Value>) -> Response {
    let files = body
        .get("files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| vec![json!({"content":body})]);
    if files.len() > 100 {
        return error(
            StatusCode::BAD_REQUEST,
            "import at most 100 accounts at once",
        );
    }
    let mut accounts = vec![];
    let mut errors = vec![];
    for file in files {
        let value = if let Some(s) = file["content"].as_str() {
            match serde_json::from_str(s) {
                Ok(v) => v,
                Err(_) => {
                    errors.push(json!({"name":file["name"],"error":"invalid JSON"}));
                    continue;
                }
            }
        } else {
            file["content"].clone()
        };
        match state
            .accounts
            .import(value, file["name"].as_str().map(str::to_owned))
            .await
        {
            Ok(a) => accounts.push(a),
            Err(e) => errors.push(json!({"name":file["name"],"error":e.to_string()})),
        }
    }
    state.refresh_models().await;
    let ids: Vec<String> = accounts.iter().map(|a| a.id.clone()).collect();
    let worker = state.clone();
    tokio::spawn(async move {
        for id in ids {
            let _ = worker.quotas.refresh(&id).await;
        }
    });
    (
        if accounts.is_empty() && !errors.is_empty() {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::OK
        },
        Json(json!({"imported":accounts.len(),"accounts":accounts,"errors":errors})),
    )
        .into_response()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountPatch {
    name: Option<String>,
    enabled: Option<bool>,
}

pub async fn update_account(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<AccountPatch>,
) -> Response {
    let name = body.name.map(|name| name.trim().to_owned());
    if name.as_ref().is_some_and(|name| {
        name.is_empty() || name.chars().count() > 120 || name.chars().any(char::is_control)
    }) {
        return error(
            StatusCode::BAD_REQUEST,
            "name must contain 1 to 120 characters without control characters",
        );
    }
    if name.is_none() && body.enabled.is_none() {
        return error(StatusCode::BAD_REQUEST, "provide a name or enabled state");
    }
    match state.accounts.update(&id, name, body.enabled).await {
        Ok(a) => Json(json!({"account":a})).into_response(),
        Err(e) => error(
            if matches!(e, crate::subscriptions::SubscriptionError::NotFound) {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            },
            &e.to_string(),
        ),
    }
}
pub async fn delete_account(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match state.accounts.delete(&id).await {
        Ok(()) => {
            state.refresh_models().await;
            Json(json!({"deleted":true})).into_response()
        }
        Err(e) => error(StatusCode::NOT_FOUND, &e.to_string()),
    }
}
pub async fn quotas(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({"quotas":state.quotas.list().await}))
}
pub async fn account_resets(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match state.resets.status(&id).await {
        Ok(status) => Json(json!(status)).into_response(),
        Err(error) => reset_error(error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetConfirmation {
    #[serde(default)]
    confirmation: bool,
    request_id: String,
}

pub async fn redeem_reset(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<ResetConfirmation>,
) -> Response {
    if !body.confirmation {
        return error(
            StatusCode::BAD_REQUEST,
            "confirm before consuming a reset credit",
        );
    }
    match state.resets.redeem(&id, &body.request_id).await {
        Ok(result) => {
            let _ = state.quotas.refresh(&id).await;
            Json(json!(result)).into_response()
        }
        Err(error) => reset_error(error),
    }
}

fn reset_error(failure: crate::subscriptions::ResetError) -> Response {
    use crate::subscriptions::{ResetError, SubscriptionError};
    let status = match &failure {
        ResetError::InvalidRequest => StatusCode::BAD_REQUEST,
        ResetError::Pending | ResetError::MissingOrganization | ResetError::HistoryFull => {
            StatusCode::CONFLICT
        }
        ResetError::Cooldown => StatusCode::TOO_MANY_REQUESTS,
        ResetError::Subscription(SubscriptionError::NotFound) => StatusCode::NOT_FOUND,
        ResetError::Subscription(SubscriptionError::Unsupported) => StatusCode::BAD_REQUEST,
        ResetError::Subscription(SubscriptionError::InvalidImport(_)) => StatusCode::BAD_REQUEST,
        ResetError::Subscription(SubscriptionError::Storage) => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::BAD_GATEWAY,
    };
    error(status, &failure.to_string())
}
pub async fn refresh_quotas(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    state.refresh_quotas(body["account_id"].as_str()).await;
    Json(json!({"quotas":state.quotas.list().await}))
}
pub async fn models(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({"models":*state.models.read().await}))
}
pub async fn refresh_models(State(state): State<Arc<AppState>>) -> Json<Value> {
    state.refresh_models().await;
    Json(json!({"models":*state.models.read().await}))
}
pub async fn set_model(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let Some(enabled) = body["enabled"].as_bool() else {
        return error(StatusCode::BAD_REQUEST, "enabled must be a boolean");
    };
    let _write = state.config_write.lock().await;
    let mut config = state.config.read().await.clone();
    if !state.models.read().await.iter().any(|m| m.id == id) {
        return error(StatusCode::NOT_FOUND, "model not found");
    }
    if enabled {
        config.disabled_models.remove(&id);
    } else {
        config.disabled_models.insert(id.clone());
    }
    if let Some(path) = &state.config_path {
        if config.save(path).await.is_err() {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot save configuration",
            );
        }
    }
    *state.config.write().await = config;
    for model in state.models.write().await.iter_mut().filter(|m| m.id == id) {
        model.enabled = enabled;
    }
    Json(json!({"id":id,"enabled":enabled})).into_response()
}
#[derive(Default, Deserialize)]
pub struct Params {
    pub limit: Option<usize>,
    pub provider: Option<String>,
    pub name: Option<String>,
    pub state: Option<String>,
}
pub async fn public_models(
    State(state): State<Arc<AppState>>,
    Query(query): Query<Params>,
) -> Json<Value> {
    let ready: BTreeSet<_> = state
        .accounts
        .list()
        .await
        .into_iter()
        .filter(|a| a.enabled)
        .map(|a| a.provider)
        .collect();
    let data:Vec<_>=state.models.read().await.iter().filter(|m|m.enabled&&ready.contains(&m.provider)&&query.provider.as_ref().is_none_or(|p|p.split(',').any(|p|p==m.provider.as_str()))).map(|m|json!({"id":m.id,"object":"model","created":0,"owned_by":m.provider.as_str(),"name":m.name,"label":m.name,"context_length":m.context_length,"thinking":m.thinking,"supported_endpoint_types":["anthropic","openai","openai-response"],"supports_websockets":true})).collect();
    Json(json!({"object":"list","data":data}))
}
pub async fn calls(State(state): State<Arc<AppState>>, Query(q): Query<Params>) -> Json<Value> {
    Json(json!({"calls":state.ledger.calls(q.limit.unwrap_or(100)).await}))
}
pub async fn summary(State(state): State<Arc<AppState>>) -> Json<Value> {
    let accounts = state.accounts.list().await;
    let l = &state.ledger;
    let client_api_base_url = state.config.read().await.server.client_api_base_url.clone();
    Json(
        json!({"accounts":accounts.len(),"enabled_accounts":accounts.iter().filter(|a|a.enabled).count(),"providers":accounts.iter().map(|a|a.provider).collect::<BTreeSet<_>>().len(),"requests":l.requests.load(Ordering::Relaxed),"errors":l.errors.load(Ordering::Relaxed),"input_tokens":l.input_tokens.load(Ordering::Relaxed),"output_tokens":l.output_tokens.load(Ordering::Relaxed),"cache_read_tokens":l.cache_read_tokens.load(Ordering::Relaxed),"cache_write_tokens":l.cache_write_tokens.load(Ordering::Relaxed),"in_flight":l.in_flight.load(Ordering::Relaxed),"dropped_logs":l.dropped_logs.load(Ordering::Relaxed),"uptime_seconds":l.started.elapsed().as_secs(),"version":env!("CARGO_PKG_VERSION"),"websocket":true,"client_api_base_url":client_api_base_url}),
    )
}
pub async fn settings(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!(state.config.read().await.settings))
}
pub async fn update_settings(
    State(state): State<Arc<AppState>>,
    Json(patch): Json<Value>,
) -> Response {
    let _write = state.config_write.lock().await;
    let mut config = state.config.read().await.clone();
    let mut value = serde_json::to_value(&config.settings).unwrap();
    let Some(fields) = patch.as_object() else {
        return error(StatusCode::BAD_REQUEST, "settings must be an object");
    };
    for (key, v) in fields {
        value[key] = v.clone();
    }
    let settings: crate::settings::Settings = match serde_json::from_value(value) {
        Ok(s) => s,
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid settings"),
    };
    if let Err(e) = settings.validate() {
        return error(StatusCode::BAD_REQUEST, &e.to_string());
    }
    config.settings = settings.clone();
    if let Some(path) = &state.config_path {
        if config.save(path).await.is_err() {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot save configuration",
            );
        }
    }
    *state.config.write().await = config;
    Json(json!(settings)).into_response()
}
pub async fn providers(State(state): State<Arc<AppState>>) -> Json<Value> {
    let mut result = json!({"providers":[
        {"id":"claude","name":"Claude","oauth":true,"quota":true,"docs_url":"https://claude.ai/settings/usage"},
        {"id":"codex","name":"Codex","oauth":true,"quota":true,"docs_url":"https://chatgpt.com/codex/settings/usage"},
        {"id":"opencode_go","name":"OpenCode Go","oauth":false,"quota":true,"docs_url":"https://opencode.ai/docs/go/"},
        {"id":"xai","name":"Grok","oauth":true,"quota":true,"docs_url":"https://grok.com"},
        {"id":"antigravity","name":"Antigravity","oauth":true,"quota":true,"docs_url":"https://antigravity.google"},
        {"id":"gemini","name":"Gemini","oauth":true,"quota":true,"docs_url":"https://geminicli.com"},
        {"id":"kimi","name":"Kimi","oauth":false,"quota":true,"docs_url":"https://kimi.com/code"},
        {"id":"qwen","name":"Qwen","oauth":false,"quota":false,"docs_url":"https://chat.qwen.ai"},
        {"id":"openai_compatible","name":"Compatible OpenAI","oauth":false,"quota":false,"docs_url":"https://platform.openai.com"}
    ]});
    for provider in result["providers"].as_array_mut().unwrap() {
        provider["oauth"] = json!(
            provider["id"]
                .as_str()
                .and_then(SubscriptionProvider::parse)
                .is_some_and(|p| state.oauth.supports(p))
        );
    }
    Json(result)
}
pub async fn oauth_start(State(state): State<Arc<AppState>>, Json(body): Json<Value>) -> Response {
    let Some(provider) = body["provider"]
        .as_str()
        .and_then(SubscriptionProvider::parse)
    else {
        return error(StatusCode::BAD_REQUEST, "unknown provider");
    };
    match state
        .oauth
        .start(provider, body["redirect_uri"].as_str().map(str::to_owned))
        .await
    {
        Ok(a) => Json(json!(a)).into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}
pub async fn oauth_status(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    match state.oauth.status(&id).await {
        Some(a) => {
            if a.status == "complete" {
                state.refresh_models().await;
                if let Some(id) = &a.account_id {
                    let worker = state.clone();
                    let id = id.clone();
                    tokio::spawn(async move {
                        let _ = worker.quotas.refresh(&id).await;
                    });
                }
            }
            Json(json!(a)).into_response()
        }
        None => error(StatusCode::NOT_FOUND, "OAuth attempt not found"),
    }
}
pub async fn oauth_callback(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Response {
    match state
        .oauth
        .complete(
            body["id"].as_str().unwrap_or(""),
            body["code"].as_str().unwrap_or(""),
            body["state"].as_str().unwrap_or(""),
        )
        .await
    {
        Ok(a) => {
            state.refresh_models().await;
            let worker = state.clone();
            let id = a.id.clone();
            tokio::spawn(async move {
                let _ = worker.quotas.refresh(&id).await;
            });
            Json(json!({"account":a})).into_response()
        }
        Err(e) => error(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

// CLIProxyAPI management discovery uses the same sanitized account data.
pub async fn auth_files(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(
        json!({"files":state.accounts.list().await.into_iter().map(|a|json!({"id":a.id,"auth_index":a.id,"name":format!("{}.json",a.id),"label":a.name,"provider":a.provider,"type":a.provider,"email":a.email,"disabled":!a.enabled,"status":a.status,"status_message":a.last_error,"created_at":a.created_at,"updated_at":a.updated_at})).collect::<Vec<_>>()}),
    )
}
pub async fn delete_auth_file(
    State(state): State<Arc<AppState>>,
    Query(q): Query<Params>,
) -> Response {
    delete_account(
        State(state),
        Path(
            q.name
                .unwrap_or_default()
                .trim_end_matches(".json")
                .to_owned(),
        ),
    )
    .await
}
pub async fn patch_auth_file(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Response {
    let id = body["name"]
        .as_str()
        .unwrap_or("")
        .trim_end_matches(".json")
        .to_owned();
    update_account(
        State(state),
        Path(id),
        Json(AccountPatch {
            name: None,
            enabled: Some(!body["disabled"].as_bool().unwrap_or(false)),
        }),
    )
    .await
}
pub async fn compatible_config(State(state): State<Arc<AppState>>) -> Json<Value> {
    let config = state.config.read().await;
    Json(
        json!({"port":config.server.listen.rsplit(':').next().and_then(|p|p.parse::<u16>().ok()),"routing":{"strategy":config.settings.strategy},"access":{"api-keys":[]},"providers":[],"websocket-auth":false,"request-retry":1,"quota-exceeded":{"switch-project":true},"version":env!("CARGO_PKG_VERSION")}),
    )
}
pub async fn version() -> Json<Value> {
    Json(json!({"version":env!("CARGO_PKG_VERSION"),"latest-version":env!("CARGO_PKG_VERSION")}))
}
pub async fn auth_file_models(
    State(state): State<Arc<AppState>>,
    Query(q): Query<Params>,
) -> Json<Value> {
    let id = q.name.unwrap_or_default();
    let account = state.accounts.get(id.trim_end_matches(".json")).await;
    let models: Vec<Value> = state
        .models
        .read()
        .await
        .iter()
        .filter(|m| account.as_ref().is_some_and(|a| a.provider == m.provider))
        .map(|m| json!({"id":m.id,"display_name":m.name,"context_length":m.context_length}))
        .collect();
    Json(json!({"models":models}))
}
