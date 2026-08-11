//! The MCP server: how an agent talks *to* douane rather than through it.
//!
//! Stateless in the 2026-07-28 sense: every request is self-contained JSON-RPC
//! over one POST, there is no session to stick to, and any instance behind any
//! load balancer could answer. Long-running work follows the Tasks extension:
//! `credential_request` returns a task handle instead of blocking, the agent
//! keeps working, and `tasks/get` answers `working` until the owner has
//! decided on the dashboard.
//!
//! **No tool result ever contains a vault value.** A tool result goes into the
//! model's context, and the context goes to whatever provider serves the next
//! turn, so putting a value in one would hand it to the exact party this
//! program exists to keep it from. An approved request answers with the
//! *placeholder* and how to redeem the grant locally; the value only ever
//! moves through the redemption path, which never transits a model.
//!
//! Authentication is the same virtual key the relay takes, in the same two
//! header forms. The client name behind the key is what a grant is filed
//! under, and an agent can only see and cancel its own requests.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::proxy::AppState;

/// How often an agent should come back, and how long a filed request stays
/// pollable. The owner is a human: seconds would hammer the broker for
/// nothing, minutes would make an approved key feel stuck.
const POLL_MS: u64 = 5_000;
const TASK_TTL_MS: u64 = 24 * 3600 * 1000;

/// How long an approval stands unless the dashboard says otherwise.
pub const DEFAULT_GRANT_HOURS: i64 = 24;

fn rpc_result(id: Value, result: Value) -> Response {
    axum::Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
}

fn rpc_error(id: Value, code: i64, message: &str) -> Response {
    axum::Json(json!({
        "jsonrpc": "2.0", "id": id,
        "error": { "code": code, "message": message }
    }))
    .into_response()
}

/// A tool-level failure: the call itself worked, the tool says no. This is the
/// shape agents actually read, where a JSON-RPC error looks like a broken
/// server rather than a refused request.
fn tool_error(id: Value, message: &str) -> Response {
    rpc_result(
        id,
        json!({
            "content": [{ "type": "text", "text": message }],
            "isError": true
        }),
    )
}

pub async fn endpoint(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    // Who is asking. Same key, same constant-time path as the relay.
    let config = state.config();
    let Some(presented) = crate::proxy::presented_key(&headers) else {
        return (StatusCode::UNAUTHORIZED, "no virtual key presented").into_response();
    };
    let Some((client_name, _)) = state.authenticate(&config, &presented) else {
        return (StatusCode::UNAUTHORIZED, "unknown virtual key").into_response();
    };
    let client = client_name.clone();

    let Ok(req) = serde_json::from_slice::<Value>(&body) else {
        return rpc_error(Value::Null, -32700, "body is not JSON");
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = req.get("method").and_then(Value::as_str) else {
        return rpc_error(id, -32600, "no method");
    };
    let params = req.get("params").cloned().unwrap_or(json!({}));

    // A notification expects nothing back. 202 is the streamable-HTTP answer.
    if req.get("id").is_none() && method.starts_with("notifications/") {
        return StatusCode::ACCEPTED.into_response();
    }

    match method {
        "initialize" => rpc_result(
            id,
            json!({
                "protocolVersion": params.get("protocolVersion")
                    .and_then(Value::as_str).unwrap_or("2026-07-28"),
                "capabilities": {
                    "tools": {},
                    "extensions": { "io.modelcontextprotocol/tasks": {} }
                },
                "serverInfo": {
                    "name": "douane",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        ),
        "ping" => rpc_result(id, json!({})),
        "tools/list" => rpc_result(id, json!({ "tools": tool_catalogue() })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match name {
                "credential_request" => credential_request(&state, &client, &args, id).await,
                "vault_deposit" => vault_deposit(&state, &client, &args, id),
                "vault_list" => rpc_result(
                    id,
                    json!({
                        "content": [{ "type": "text",
                            "text": serde_json::to_string_pretty(&state.vault_list())
                                    .unwrap_or_default() }]
                    }),
                ),
                other => rpc_error(id, -32602, &format!("no tool named `{other}`")),
            }
        }
        "tasks/get" => tasks_get(&state, &client, &params, id),
        "tasks/cancel" => {
            let Some(task_id) = params.get("taskId").and_then(Value::as_str) else {
                return rpc_error(id, -32602, "taskId is required");
            };
            match state.audit.grant_cancel(task_id, &client) {
                Ok(true) => rpc_result(id, json!({ "status": "cancelled" })),
                Ok(false) => rpc_error(
                    id,
                    -32602,
                    "no pending task of yours under that id: it may already be decided",
                ),
                Err(e) => {
                    tracing::error!("grant cancel failed: {e}");
                    rpc_error(id, -32603, "the request log could not be written")
                }
            }
        }
        other => rpc_error(id, -32601, &format!("method `{other}` is not served here")),
    }
}

/// File the request, tell the owner, hand back a task. Never blocks on the
/// human: that is the entire reason the Tasks extension is used.
async fn credential_request(
    state: &Arc<AppState>,
    client: &str,
    args: &Value,
    id: Value,
) -> Response {
    let Some(reference) = args.get("reference").and_then(Value::as_str) else {
        return tool_error(id, "`reference` is required, as `namespace.key`");
    };
    let Some(reason) = args.get("reason").and_then(Value::as_str).filter(|r| !r.trim().is_empty())
    else {
        return tool_error(
            id,
            "`reason` is required: the owner decides on it, so say what the credential is for",
        );
    };
    let Ok(r) = douane_vault::SecretRef::parse(reference) else {
        return tool_error(id, "a reference is `namespace.key`");
    };
    if state.secret(&r).is_none() {
        // Naming what exists is fine: `vault_list` already does. What matters
        // is failing now rather than making the owner approve a ghost.
        return tool_error(id, "no such vault entry: `vault_list` says what exists");
    }
    if crate::dash::operating_refs(state).contains(&r.to_string()) {
        return tool_error(
            id,
            "that reference is what douane itself runs on, and is not grantable",
        );
    }

    // Random enough that one agent cannot guess another's handle. The handle
    // gates a status and a placeholder, never a value, so 128 bits of OS
    // entropy is comfortably past what it protects.
    let task_id = {
        use rand::RngCore as _;
        let mut bytes = [0u8; 16];
        rand::rng().fill_bytes(&mut bytes);
        format!("task_{}", hex::encode(bytes))
    };

    if let Err(e) = state.audit.grant_create(&task_id, client, reference, reason) {
        tracing::error!("grant create failed: {e}");
        return rpc_error(id, -32603, "the request could not be filed");
    }
    tracing::info!(client, reference, "credential access requested");
    notify(
        state,
        &format!("douane: `{client}` asks for `{reference}`: {reason}"),
    )
    .await;

    rpc_result(
        id,
        json!({
            "content": [{ "type": "text",
                "text": format!(
                    "Request filed for `{reference}`. Keep working and poll \
                     tasks/get with this taskId; the owner has been notified."
                ) }],
            "task": {
                "taskId": task_id,
                "status": "working",
                "pollIntervalMs": POLL_MS,
                "ttlMs": TASK_TTL_MS
            }
        }),
    )
}

/// Write-only deposit. An agent can file a credential it created; it can never
/// read one back, and it cannot overwrite what already exists.
fn vault_deposit(state: &Arc<AppState>, client: &str, args: &Value, id: Value) -> Response {
    let Some(reference) = args.get("reference").and_then(Value::as_str) else {
        return tool_error(id, "`reference` is required, as `namespace.key`");
    };
    let Some(value) = args.get("value").and_then(Value::as_str).filter(|v| !v.is_empty()) else {
        return tool_error(id, "`value` is required and non-empty");
    };
    let Ok(r) = douane_vault::SecretRef::parse(reference) else {
        return tool_error(id, "a reference is `namespace.key`");
    };
    if value.len() > crate::dash::MAX_SECRET_BYTES {
        return tool_error(id, "that is too large to be a credential");
    }
    if state.secret(&r).is_some() {
        // Replacing an existing entry from an agent would be a way to vandalise
        // the vault, or worse, to swap a watched value for an unwatched one.
        return tool_error(
            id,
            "an entry already exists there: replacing one is the owner's move, on the dashboard",
        );
    }

    let mut entry = douane_vault::Entry::new(douane_vault::Secret::new(value.to_owned()));
    entry.note = args
        .get("note")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or(Some(format!("deposited over MCP by `{client}`")));
    match state.vault_put(&r, entry) {
        Ok(()) => {
            tracing::info!(secret = %r, client, "a credential was deposited over MCP");
            rpc_result(
                id,
                json!({
                    "content": [{ "type": "text",
                        "text": format!(
                            "Stored at `{r}` and now watched by the scrubber. \
                             Its placeholder is `{}`.", r.placeholder()
                        ) }]
                }),
            )
        }
        Err(e) => {
            tracing::error!("vault write failed: {e}");
            rpc_error(id, -32603, "the vault could not be written")
        }
    }
}

/// Where a filed request stands. The placeholder is the most a completed task
/// ever carries.
fn tasks_get(state: &Arc<AppState>, client: &str, params: &Value, id: Value) -> Response {
    let Some(task_id) = params.get("taskId").and_then(Value::as_str) else {
        return rpc_error(id, -32602, "taskId is required");
    };
    let grant = match state.audit.grant_by_task(task_id) {
        Ok(g) => g,
        Err(e) => {
            tracing::error!("grant lookup failed: {e}");
            return rpc_error(id, -32603, "the request log could not be read");
        }
    };
    // A grant that is not yours does not exist, as far as you can tell: saying
    // "not yours" would confirm the handle.
    let Some(g) = grant.filter(|g| g.client == client) else {
        return rpc_error(id, -32602, "no task of yours under that id");
    };

    let task = match g.status.as_str() {
        "pending" => json!({
            "taskId": g.task_id, "status": "working",
            "pollIntervalMs": POLL_MS,
            "statusMessage": "waiting for the owner's decision"
        }),
        "approved" => {
            let placeholder = douane_vault::SecretRef::parse(&g.reference)
                .map(|r| r.placeholder())
                .unwrap_or_default();
            json!({
                "taskId": g.task_id, "status": "completed",
                "result": {
                    "content": [{ "type": "text",
                        "text": format!(
                            "Approved until {}. Use the placeholder `{placeholder}` \
                             wherever the value is needed; redeem it locally with \
                             `douanectl use {}` (the value itself never travels \
                             through this channel).",
                            g.expires_at.as_deref().unwrap_or("revoked"),
                            g.reference
                        ) }],
                    "reference": g.reference,
                    "placeholder": placeholder,
                    "expires_at": g.expires_at
                }
            })
        }
        "denied" => json!({
            "taskId": g.task_id, "status": "failed",
            "error": { "message": format!(
                "denied by the owner{}",
                g.note.as_deref().map(|n| format!(": {n}")).unwrap_or_default()
            ) }
        }),
        "cancelled" => json!({ "taskId": g.task_id, "status": "cancelled" }),
        other => json!({ "taskId": g.task_id, "status": other }),
    };
    rpc_result(id, json!({ "task": task }))
}

/// Redeem an approved grant: `POST /agent/api/use` with the virtual key and
/// `{"reference": "ns.key"}`.
///
/// **This is the one surface in douane that answers with a value**, and it is
/// the deliberate, load-bearing exception to the no-read rule. What makes it
/// tolerable where every other read surface is banned: the value only moves
/// after a human approved this client for this reference, only until the
/// approval expires, only to the client that asked, and every redemption is
/// stamped on the grant. The caller is `douanectl use`, a local process that
/// puts the value in a child's environment; the value never enters a model's
/// context on this path, which is the property the whole design defends.
pub async fn redeem(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let config = state.config();
    let Some(presented) = crate::proxy::presented_key(&headers) else {
        return (StatusCode::UNAUTHORIZED, "no virtual key presented").into_response();
    };
    let Some((client_name, _)) = state.authenticate(&config, &presented) else {
        return (StatusCode::UNAUTHORIZED, "unknown virtual key").into_response();
    };
    let client = client_name.clone();

    let reference = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v.get("reference").and_then(Value::as_str).map(str::to_owned));
    let Some(reference) = reference else {
        return crate::proxy::json_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "the body is `{\"reference\": \"namespace.key\"}`",
        );
    };
    let Ok(r) = douane_vault::SecretRef::parse(&reference) else {
        return crate::proxy::json_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "a reference is `namespace.key`",
        );
    };

    use crate::audit::Redeem;
    match state.audit.grant_redeem(&client, &reference) {
        Ok(Redeem::Approved(g)) => {
            let Some(secret) = state.secret(&r) else {
                // Approved, then deleted from the vault: the approval outlived
                // the thing it approved.
                return crate::proxy::json_error(
                    StatusCode::GONE,
                    "not_found_error",
                    "the entry behind that approval is no longer in the vault",
                );
            };
            tracing::info!(client, reference, "an approved credential was redeemed");
            axum::Json(json!({
                "reference": reference,
                "value": secret.expose(),
                "expires_at": g.expires_at,
            }))
            .into_response()
        }
        Ok(Redeem::Pending) => crate::proxy::json_error(
            StatusCode::FORBIDDEN,
            "permission_error",
            "still waiting on the owner: poll tasks/get rather than this endpoint",
        ),
        Ok(Redeem::Denied(note)) => crate::proxy::json_error(
            StatusCode::FORBIDDEN,
            "permission_error",
            &format!(
                "the owner said no{}",
                note.map(|n| format!(": {n}")).unwrap_or_default()
            ),
        ),
        Ok(Redeem::Expired) => crate::proxy::json_error(
            StatusCode::GONE,
            "permission_error",
            "that approval has lapsed: file a new credential_request",
        ),
        Ok(Redeem::NoRequest) => crate::proxy::json_error(
            StatusCode::NOT_FOUND,
            "not_found_error",
            "no grant of yours on that reference: file a credential_request first",
        ),
        Err(e) => {
            tracing::error!("redeem lookup failed: {e}");
            crate::proxy::json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "api_error",
                "the request log could not be read",
            )
        }
    }
}

/// One line to the owner's phone, best effort. Failure is logged and swallowed:
/// a notification channel that can fail a request is a denial-of-service on
/// the thing it announces.
async fn notify(state: &Arc<AppState>, text: &str) {
    let Some(url) = state.base.server.notify_url.clone() else {
        return;
    };
    let res = state
        .http
        .post(&url)
        .body(text.to_owned())
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await;
    if let Err(e) = res {
        tracing::warn!("notification not delivered: {e}");
    }
}

fn tool_catalogue() -> Value {
    json!([
        {
            "name": "credential_request",
            "description": "Ask the owner for access to a vault credential. Returns a task \
                            immediately; keep working and poll tasks/get. An approval hands \
                            back the placeholder, never the value.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "reference": { "type": "string", "description": "namespace.key" },
                    "reason": { "type": "string", "description": "what the credential is for; the owner decides on this" }
                },
                "required": ["reference", "reason"]
            }
        },
        {
            "name": "vault_deposit",
            "description": "File a new credential into the vault, write-only. It becomes \
                            watched by the scrubber at once and cannot be read back.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "reference": { "type": "string", "description": "namespace.key" },
                    "value": { "type": "string" },
                    "note": { "type": "string" }
                },
                "required": ["reference", "value"]
            }
        },
        {
            "name": "vault_list",
            "description": "What the vault holds: names, placeholders, fingerprints. No values.",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ])
}
