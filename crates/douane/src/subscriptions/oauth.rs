use super::google::GoogleOAuth;
use super::store::{apply_identity, text};
use super::{
    AccountCredential, AccountStore, AccountSummary, SubscriptionError, SubscriptionProvider,
    parse_timestamp, random_id, timestamp,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::sync::Mutex;

const CODEX_CLIENT: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CLAUDE_CLIENT: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const CLAUDE_SCOPE: &str =
    "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
const XAI_CLIENT: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const XAI_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const XAI_DISCOVERY: &str = "https://auth.x.ai/.well-known/openid-configuration";

#[derive(Clone, Debug, Serialize)]
pub struct OAuthAttempt {
    pub id: String,
    pub provider: SubscriptionProvider,
    pub url: String,
    pub state: String,
    pub redirect_uri: String,
    pub expires_at: String,
    pub status: String,
    pub account_id: Option<String>,
    pub error: Option<String>,
    pub user_code: Option<String>,
    pub interval: Option<u64>,
}

struct Pending {
    public: OAuthAttempt,
    verifier: String,
    device: Option<DeviceFlow>,
}

#[derive(Clone)]
struct DeviceFlow {
    code: String,
    token_endpoint: String,
    next_poll: time::OffsetDateTime,
    interval: u64,
}

pub struct OAuthManager {
    client: reqwest::Client,
    store: Arc<AccountStore>,
    pending: Mutex<BTreeMap<String, Pending>>,
    codex_token_url: String,
    claude_token_url: String,
    google: GoogleOAuth,
}

impl OAuthManager {
    pub fn new(client: reqwest::Client, store: Arc<AccountStore>) -> Self {
        let google = GoogleOAuth::load(client.clone(), &store.oauth_clients_path());
        Self {
            client,
            store,
            pending: Mutex::new(BTreeMap::new()),
            codex_token_url: "https://auth.openai.com/oauth/token".into(),
            claude_token_url: "https://platform.claude.com/v1/oauth/token".into(),
            google,
        }
    }

    pub fn supports(&self, provider: SubscriptionProvider) -> bool {
        match provider {
            SubscriptionProvider::Codex
            | SubscriptionProvider::Claude
            | SubscriptionProvider::Xai => true,
            SubscriptionProvider::Gemini | SubscriptionProvider::Antigravity => {
                self.google.supports(provider)
            }
            _ => false,
        }
    }

    pub async fn start(
        &self,
        provider: SubscriptionProvider,
        redirect_uri: Option<String>,
    ) -> Result<OAuthAttempt, SubscriptionError> {
        if provider == SubscriptionProvider::Xai {
            return self.start_xai().await;
        }
        if matches!(
            provider,
            SubscriptionProvider::Gemini | SubscriptionProvider::Antigravity
        ) {
            return self.start_google(provider, redirect_uri).await;
        }
        let (auth_url, client_id, default_redirect, scope) = match provider {
            SubscriptionProvider::Codex => (
                "https://auth.openai.com/oauth/authorize",
                CODEX_CLIENT,
                "http://localhost:1455/auth/callback",
                "openid email profile offline_access",
            ),
            SubscriptionProvider::Claude => (
                "https://claude.ai/oauth/authorize",
                CLAUDE_CLIENT,
                "http://localhost:54545/callback",
                CLAUDE_SCOPE,
            ),
            _ => return Err(SubscriptionError::Unsupported),
        };
        let redirect = redirect_uri.unwrap_or_else(|| default_redirect.into());
        validate_redirect(&redirect)?;
        let state = random_id();
        let verifier = format!("{}{}", random_id(), random_id());
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut url =
            reqwest::Url::parse(auth_url).map_err(|_| SubscriptionError::InvalidResponse)?;
        url.query_pairs_mut().extend_pairs([
            ("client_id", client_id),
            ("response_type", "code"),
            ("redirect_uri", redirect.as_str()),
            ("scope", scope),
            ("state", state.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ]);
        match provider {
            SubscriptionProvider::Codex => {
                url.query_pairs_mut().extend_pairs([
                    ("prompt", "login"),
                    ("id_token_add_organizations", "true"),
                    ("codex_cli_simplified_flow", "true"),
                ]);
            }
            SubscriptionProvider::Claude => {
                url.query_pairs_mut().append_pair("code", "true");
            }
            _ => {}
        }
        let attempt = OAuthAttempt {
            id: random_id(),
            provider,
            url: url.into(),
            state,
            redirect_uri: redirect,
            expires_at: timestamp(time::OffsetDateTime::now_utc() + time::Duration::minutes(15)),
            status: "pending".into(),
            account_id: None,
            error: None,
            user_code: None,
            interval: None,
        };
        self.save_attempt(Pending {
            public: attempt.clone(),
            verifier,
            device: None,
        })
        .await;
        Ok(attempt)
    }

    async fn start_google(
        &self,
        provider: SubscriptionProvider,
        redirect_uri: Option<String>,
    ) -> Result<OAuthAttempt, SubscriptionError> {
        let default_redirect = match provider {
            SubscriptionProvider::Gemini => "https://codeassist.google.com/authcode",
            SubscriptionProvider::Antigravity => "http://localhost:51121/oauth-callback",
            _ => return Err(SubscriptionError::Unsupported),
        };
        let redirect = redirect_uri.unwrap_or_else(|| default_redirect.into());
        if provider != SubscriptionProvider::Gemini
            || redirect != "https://codeassist.google.com/authcode"
        {
            validate_redirect(&redirect)?;
        }
        let state = random_id();
        let verifier = format!("{}{}", random_id(), random_id());
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let url = self
            .google
            .authorize(provider, &state, &redirect, &challenge)?;
        let attempt = OAuthAttempt {
            id: random_id(),
            provider,
            url,
            state,
            redirect_uri: redirect,
            expires_at: timestamp(time::OffsetDateTime::now_utc() + time::Duration::minutes(15)),
            status: "pending".into(),
            account_id: None,
            error: None,
            user_code: None,
            interval: None,
        };
        self.save_attempt(Pending {
            public: attempt.clone(),
            verifier,
            device: None,
        })
        .await;
        Ok(attempt)
    }

    async fn save_attempt(&self, pending: Pending) {
        let mut attempts = self.pending.lock().await;
        let now = time::OffsetDateTime::now_utc();
        attempts.retain(|_, attempt| {
            parse_timestamp(&attempt.public.expires_at)
                .is_some_and(|t| t + time::Duration::hours(1) > now)
        });
        if attempts.len() >= 64 {
            if let Some(oldest) = attempts
                .iter()
                .min_by_key(|(_, p)| &p.public.expires_at)
                .map(|(id, _)| id.clone())
            {
                attempts.remove(&oldest);
            }
        }
        attempts.insert(pending.public.id.clone(), pending);
    }

    pub async fn status(&self, id: &str) -> Option<OAuthAttempt> {
        self.poll_device(id).await;
        let mut attempts = self.pending.lock().await;
        let attempt = attempts.get_mut(id)?;
        if attempt.public.status == "pending"
            && parse_timestamp(&attempt.public.expires_at)
                .is_none_or(|t| t <= time::OffsetDateTime::now_utc())
        {
            attempt.public.status = "expired".into();
            attempt.device = None;
            attempt.verifier.clear();
        }
        Some(attempt.public.clone())
    }

    pub async fn complete(
        &self,
        id: &str,
        code: &str,
        state: &str,
    ) -> Result<AccountSummary, SubscriptionError> {
        let (code, state) = callback_fields(code, state)?;
        let (provider, redirect, verifier) = {
            let mut attempts = self.pending.lock().await;
            let pending = attempts.get_mut(id).ok_or(SubscriptionError::Expired)?;
            if pending.public.status != "pending"
                || pending.device.is_some()
                || parse_timestamp(&pending.public.expires_at)
                    .is_none_or(|t| t <= time::OffsetDateTime::now_utc())
            {
                return Err(SubscriptionError::Expired);
            }
            if state != pending.public.state {
                return Err(SubscriptionError::InvalidState);
            }
            pending.public.status = "exchanging".into();
            (
                pending.public.provider,
                pending.public.redirect_uri.clone(),
                std::mem::take(&mut pending.verifier),
            )
        };
        let result = self
            .exchange(provider, &code, &state, &redirect, &verifier)
            .await;
        self.finish_attempt(id, &result).await;
        result
    }

    async fn exchange(
        &self,
        provider: SubscriptionProvider,
        code: &str,
        state: &str,
        redirect: &str,
        verifier: &str,
    ) -> Result<AccountSummary, SubscriptionError> {
        if matches!(
            provider,
            SubscriptionProvider::Gemini | SubscriptionProvider::Antigravity
        ) {
            let value = self
                .google
                .exchange(provider, code, redirect, verifier)
                .await?;
            return self.store.import(value, None).await;
        }
        let request = match provider {
            SubscriptionProvider::Codex => self.client.post(&self.codex_token_url).form(&[
                ("grant_type", "authorization_code"),
                ("client_id", CODEX_CLIENT),
                ("code", code),
                ("redirect_uri", redirect),
                ("code_verifier", verifier),
            ]),
            SubscriptionProvider::Claude => {
                #[derive(Serialize)]
                struct ClaudeExchange<'a> {
                    grant_type: &'a str,
                    code: &'a str,
                    redirect_uri: &'a str,
                    client_id: &'a str,
                    code_verifier: &'a str,
                    state: &'a str,
                }
                self.client
                    .post(&self.claude_token_url)
                    .header("User-Agent", "axios/1.15.2")
                    .json(&ClaudeExchange {
                        grant_type: "authorization_code",
                        code,
                        redirect_uri: redirect,
                        client_id: CLAUDE_CLIENT,
                        code_verifier: verifier,
                        state,
                    })
            }
            _ => return Err(SubscriptionError::Unsupported),
        };
        let response = send_json(request).await?;
        let value = normalized_tokens(provider, response, None)?;
        self.store.import(value, None).await
    }

    async fn finish_attempt(&self, id: &str, result: &Result<AccountSummary, SubscriptionError>) {
        if let Some(attempt) = self.pending.lock().await.get_mut(id) {
            attempt.device = None;
            attempt.verifier.clear();
            match result {
                Ok(account) => {
                    attempt.public.status = "complete".into();
                    attempt.public.account_id = Some(account.id.clone());
                }
                Err(error) => {
                    attempt.public.status = "error".into();
                    attempt.public.error = Some(error.to_string());
                }
            }
        }
    }

    pub async fn credential(&self, id: &str) -> Result<AccountCredential, SubscriptionError> {
        self.refresh(id, false).await
    }

    pub async fn force_refresh(&self, id: &str) -> Result<AccountCredential, SubscriptionError> {
        self.refresh(id, true).await
    }

    async fn refresh(&self, id: &str, force: bool) -> Result<AccountCredential, SubscriptionError> {
        let initial = self
            .store
            .get(id)
            .await
            .ok_or(SubscriptionError::NotFound)?;
        if !force && !initial.needs_refresh() {
            return Ok(initial);
        }
        let lock = self
            .store
            .refresh_locks
            .lock()
            .await
            .entry(id.into())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;
        let mut account = self
            .store
            .get(id)
            .await
            .ok_or(SubscriptionError::NotFound)?;
        if (!force && !account.needs_refresh())
            || (force && account.access_token != initial.access_token)
        {
            return Ok(account);
        }
        let refresh_token = account
            .refresh_token
            .as_deref()
            .ok_or(SubscriptionError::NoRefreshToken)?;
        let request = match account.provider {
            SubscriptionProvider::Codex => self.client.post(&self.codex_token_url).form(&[
                ("grant_type", "refresh_token"), ("client_id", CODEX_CLIENT), ("refresh_token", refresh_token), ("scope", "openid profile email"),
            ]),
            SubscriptionProvider::Claude => self.client.post(&self.claude_token_url).header("User-Agent", "axios/1.15.2")
                .json(&json!({"grant_type":"refresh_token","client_id":CLAUDE_CLIENT,"refresh_token":refresh_token,"scope":CLAUDE_SCOPE})),
            SubscriptionProvider::Xai => {
                let endpoint = match &account.token_endpoint { Some(endpoint) => validate_xai_endpoint(endpoint)?, None => self.xai_discovery().await?.1 };
                self.client.post(endpoint).form(&[("grant_type", "refresh_token"), ("client_id", XAI_CLIENT), ("refresh_token", refresh_token)])
            }
            SubscriptionProvider::Gemini | SubscriptionProvider::Antigravity => {
                self.google.refresh_request(account.provider, refresh_token, account.client_id.as_deref(), account.client_secret.as_deref())?
            }
            SubscriptionProvider::Kimi => self.client.post("https://auth.kimi.com/api/oauth/token")
                .form(&[("grant_type", "refresh_token"), ("client_id", "17e5f671-d194-4dfb-9706-5516cb48c098"), ("refresh_token", refresh_token)]),
            _ => return Err(SubscriptionError::Unsupported),
        };
        let result = send_json(request).await;
        let response = match result {
            Ok(response) => response,
            Err(error) => {
                let _ = self.store.mark_error(id, Some(error.to_string())).await;
                return Err(error);
            }
        };
        account.access_token =
            text(&response, &["access_token"]).ok_or(SubscriptionError::InvalidResponse)?;
        if let Some(rotated) = text(&response, &["refresh_token"]) {
            account.refresh_token = Some(rotated);
        }
        if let Some(token) = text(&response, &["id_token"]) {
            account.id_token = Some(token);
        }
        account.expires_at = expiry_from_response(&response);
        account.last_error = None;
        account.cooldown_until = None;
        apply_identity(&mut account);
        self.store.update_credential(account).await?;
        self.store.get(id).await.ok_or(SubscriptionError::NotFound)
    }

    async fn xai_discovery(&self) -> Result<(String, String), SubscriptionError> {
        let response = send_json(self.client.get(XAI_DISCOVERY)).await?;
        let device = text(&response, &["device_authorization_endpoint"])
            .ok_or(SubscriptionError::InvalidResponse)?;
        let token =
            text(&response, &["token_endpoint"]).ok_or(SubscriptionError::InvalidResponse)?;
        Ok((
            validate_xai_endpoint(&device)?,
            validate_xai_endpoint(&token)?,
        ))
    }

    async fn start_xai(&self) -> Result<OAuthAttempt, SubscriptionError> {
        let (device_endpoint, token_endpoint) = self.xai_discovery().await?;
        let response = send_json(
            self.client
                .post(device_endpoint)
                .form(&[("client_id", XAI_CLIENT), ("scope", XAI_SCOPE)]),
        )
        .await?;
        let device_code =
            text(&response, &["device_code"]).ok_or(SubscriptionError::InvalidResponse)?;
        let user_code =
            text(&response, &["user_code"]).ok_or(SubscriptionError::InvalidResponse)?;
        let url = text(
            &response,
            &["verification_uri_complete", "verification_uri"],
        )
        .ok_or(SubscriptionError::InvalidResponse)?;
        validate_xai_endpoint(&url)?;
        let interval = response
            .get("interval")
            .and_then(Value::as_u64)
            .unwrap_or(5)
            .clamp(5, 60);
        let expires = response
            .get("expires_in")
            .and_then(Value::as_i64)
            .unwrap_or(900)
            .clamp(1, 1800);
        let attempt = OAuthAttempt {
            id: random_id(),
            provider: SubscriptionProvider::Xai,
            url,
            state: random_id(),
            redirect_uri: String::new(),
            expires_at: timestamp(
                time::OffsetDateTime::now_utc() + time::Duration::seconds(expires),
            ),
            status: "pending".into(),
            account_id: None,
            error: None,
            user_code: Some(user_code),
            interval: Some(interval),
        };
        self.save_attempt(Pending {
            public: attempt.clone(),
            verifier: String::new(),
            device: Some(DeviceFlow {
                code: device_code,
                token_endpoint,
                next_poll: time::OffsetDateTime::now_utc(),
                interval,
            }),
        })
        .await;
        Ok(attempt)
    }

    async fn poll_device(&self, id: &str) {
        let flow = {
            let mut attempts = self.pending.lock().await;
            let Some(pending) = attempts.get_mut(id) else {
                return;
            };
            if pending.public.status != "pending"
                || parse_timestamp(&pending.public.expires_at)
                    .is_none_or(|t| t <= time::OffsetDateTime::now_utc())
            {
                return;
            }
            let Some(flow) = pending.device.as_mut() else {
                return;
            };
            if flow.next_poll > time::OffsetDateTime::now_utc() {
                return;
            }
            // Reserve a full HTTP timeout so overlapping dashboard polls cannot
            // consume a single device code simultaneously.
            flow.next_poll = time::OffsetDateTime::now_utc() + time::Duration::seconds(31);
            flow.clone()
        };
        let response = self
            .client
            .post(&flow.token_endpoint)
            .timeout(Duration::from_secs(30))
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", XAI_CLIENT),
                ("device_code", flow.code.as_str()),
            ])
            .send()
            .await;
        let result = match response {
            Ok(response) if response.status().is_success() => {
                match response.json::<Value>().await {
                    Ok(value) => match normalized_tokens(
                        SubscriptionProvider::Xai,
                        value,
                        Some(&flow.token_endpoint),
                    ) {
                        Ok(value) => self.store.import(value, None).await,
                        Err(error) => Err(error),
                    },
                    Err(_) => Err(SubscriptionError::InvalidResponse),
                }
            }
            Ok(response) => {
                let status = response.status().as_u16();
                let error = response
                    .json::<Value>()
                    .await
                    .ok()
                    .and_then(|v| text(&v, &["error"]));
                if matches!(
                    error.as_deref(),
                    Some("authorization_pending" | "slow_down")
                ) {
                    let mut attempts = self.pending.lock().await;
                    if let Some(device) = attempts.get_mut(id).and_then(|p| p.device.as_mut()) {
                        if error.as_deref() == Some("slow_down") {
                            device.interval = (device.interval + 5).min(60);
                        }
                        device.next_poll = time::OffsetDateTime::now_utc()
                            + time::Duration::seconds(device.interval as i64);
                    }
                    return;
                }
                Err(SubscriptionError::Upstream(status))
            }
            Err(_) => Err(SubscriptionError::Transport),
        };
        self.finish_attempt(id, &result).await;
    }
}

pub(crate) async fn send_json(
    request: reqwest::RequestBuilder,
) -> Result<Value, SubscriptionError> {
    let response = request
        .timeout(Duration::from_secs(30))
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|_| SubscriptionError::Transport)?;
    if !response.status().is_success() {
        return Err(SubscriptionError::Upstream(response.status().as_u16()));
    }
    response
        .json::<Value>()
        .await
        .map_err(|_| SubscriptionError::InvalidResponse)
}

fn expiry_from_response(value: &Value) -> Option<String> {
    value
        .get("expires_in")
        .and_then(Value::as_i64)
        .filter(|seconds| *seconds > 0)
        .map(|seconds| {
            timestamp(
                time::OffsetDateTime::now_utc() + time::Duration::seconds(seconds.min(31_536_000)),
            )
        })
}

fn normalized_tokens(
    provider: SubscriptionProvider,
    mut value: Value,
    token_endpoint: Option<&str>,
) -> Result<Value, SubscriptionError> {
    if text(&value, &["access_token"]).is_none() {
        return Err(SubscriptionError::InvalidResponse);
    }
    let expires = expiry_from_response(&value);
    let email = value
        .get("account")
        .and_then(|a| text(a, &["email_address", "email"]));
    let account_id = value.get("account").and_then(|a| text(a, &["uuid"]));
    let object = value
        .as_object_mut()
        .ok_or(SubscriptionError::InvalidResponse)?;
    object.insert("type".into(), json!(provider.as_str()));
    if let Some(expires) = expires {
        object.insert("expired".into(), json!(expires));
    }
    if let Some(email) = email {
        object.insert("email".into(), json!(email));
    }
    if let Some(id) = account_id {
        object.insert("account_id".into(), json!(id));
    }
    if let Some(endpoint) = token_endpoint {
        object.insert("token_endpoint".into(), json!(endpoint));
    }
    if provider == SubscriptionProvider::Xai {
        object.insert(
            "base_url".into(),
            json!("https://cli-chat-proxy.grok.com/v1"),
        );
    }
    Ok(value)
}

fn callback_fields(
    code: &str,
    supplied_state: &str,
) -> Result<(String, String), SubscriptionError> {
    let code = code.trim();
    let mut state = supplied_state.trim().to_owned();
    let code = if code.starts_with("http://") || code.starts_with("https://") {
        let url = reqwest::Url::parse(code).map_err(|_| SubscriptionError::InvalidResponse)?;
        let mut found = None;
        for (key, value) in url.query_pairs() {
            if key == "code" {
                found = Some(value.into_owned());
            } else if key == "state" {
                if !state.is_empty() && state != value {
                    return Err(SubscriptionError::InvalidState);
                }
                state = value.into_owned();
            }
        }
        found.ok_or(SubscriptionError::InvalidResponse)?
    } else if let Some((code, fragment_state)) = code.split_once('#') {
        if !state.is_empty() && state != fragment_state {
            return Err(SubscriptionError::InvalidState);
        }
        state = fragment_state.into();
        code.into()
    } else {
        code.into()
    };
    if code.is_empty() || state.is_empty() || code.len() > 8192 {
        return Err(SubscriptionError::InvalidResponse);
    }
    Ok((code, state))
}

fn validate_redirect(redirect: &str) -> Result<(), SubscriptionError> {
    let url = reqwest::Url::parse(redirect).map_err(|_| SubscriptionError::InvalidRedirect)?;
    if url.scheme() != "http"
        || !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(SubscriptionError::InvalidRedirect);
    }
    Ok(())
}

fn validate_xai_endpoint(endpoint: &str) -> Result<String, SubscriptionError> {
    let url = reqwest::Url::parse(endpoint).map_err(|_| SubscriptionError::InvalidResponse)?;
    let host = url.host_str().unwrap_or_default();
    if url.scheme() != "https"
        || !(host == "x.ai" || host.ends_with(".x.ai"))
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(SubscriptionError::InvalidResponse);
    }
    Ok(endpoint.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_pasted_callback_and_claude_fragment_with_state_validation() {
        assert_eq!(
            callback_fields("http://localhost:1455/auth/callback?code=abc&state=xyz", "").unwrap(),
            ("abc".into(), "xyz".into())
        );
        assert_eq!(
            callback_fields("code#state", "state").unwrap(),
            ("code".into(), "state".into())
        );
        assert!(matches!(
            callback_fields("code#wrong", "state"),
            Err(SubscriptionError::InvalidState)
        ));
        assert!(validate_redirect("https://unrelated.example/callback").is_err());
        assert!(validate_xai_endpoint("https://auth.x.ai.evil.example/token").is_err());
    }

    #[tokio::test]
    async fn browser_authorization_uses_pkce_and_rejects_wrong_state_before_network() {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(AccountStore::open(directory.path()).unwrap());
        let manager = OAuthManager::new(reqwest::Client::new(), store);
        let attempt = manager
            .start(SubscriptionProvider::Codex, None)
            .await
            .unwrap();
        let url = reqwest::Url::parse(&attempt.url).unwrap();
        let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(query["state"], attempt.state);
        assert_eq!(query["code_challenge"].len(), 43);
        assert!(matches!(
            manager.complete(&attempt.id, "fixture-code", "wrong").await,
            Err(SubscriptionError::InvalidState)
        ));
        assert_eq!(manager.status(&attempt.id).await.unwrap().status, "pending");
    }

    #[tokio::test]
    async fn refresh_rotation_is_serialized_across_managers_and_persisted() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let server_calls = calls.clone();
        let app = axum::Router::new().route("/token", axum::routing::post(move || {
            let calls = server_calls.clone();
            async move {
                let index = calls.fetch_add(1, Ordering::SeqCst) + 1;
                tokio::time::sleep(Duration::from_millis(40)).await;
                axum::Json(json!({"access_token":format!("new-access-{index}"), "refresh_token":format!("rotated-{index}"), "expires_in":3600}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(AccountStore::open(directory.path()).unwrap());
        let summary = store.import(json!({"type":"codex","access_token":"old","refresh_token":"old-refresh","expired":"2020-01-01T00:00:00Z"}), None).await.unwrap();
        let mut first = OAuthManager::new(reqwest::Client::new(), store.clone());
        first.codex_token_url = endpoint.clone();
        let mut second = OAuthManager::new(reqwest::Client::new(), store.clone());
        second.codex_token_url = endpoint;
        let (one, two) = tokio::join!(
            first.credential(&summary.id),
            second.credential(&summary.id)
        );
        assert_eq!(one.unwrap().access_token, "new-access-1");
        assert_eq!(two.unwrap().access_token, "new-access-1");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let (one, two) = tokio::join!(
            first.force_refresh(&summary.id),
            second.force_refresh(&summary.id)
        );
        assert_eq!(one.unwrap().access_token, "new-access-2");
        assert_eq!(two.unwrap().access_token, "new-access-2");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let reopened = AccountStore::open(directory.path()).unwrap();
        assert_eq!(
            reopened
                .get(&summary.id)
                .await
                .unwrap()
                .refresh_token
                .as_deref(),
            Some("rotated-2")
        );
        task.abort();
        let _ = task.await;
    }
}
