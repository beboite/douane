//! Google's installed-app OAuth and Code Assist account onboarding.
//! Public-client metadata comes from private runtime configuration, not source.

use super::oauth::send_json;
use super::store::text;
use super::{SubscriptionError, SubscriptionProvider, timestamp};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, time::Duration};

const ANTIGRAVITY_CLIENT: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
const GEMINI_CLIENT: &str =
    "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";

#[derive(Clone)]
struct ClientMetadata {
    id: String,
    secret: String,
    project: Option<String>,
}

pub(crate) struct GoogleOAuth {
    client: reqwest::Client,
    clients: BTreeMap<SubscriptionProvider, ClientMetadata>,
    token_url: String,
    userinfo_url: String,
    codeassist_url: String,
    daily_url: String,
    poll_delay: Duration,
}

impl GoogleOAuth {
    pub(crate) fn load(client: reqwest::Client, path: &Path) -> Self {
        let file: Value = std::fs::metadata(path)
            .ok()
            .filter(|metadata| metadata.len() < 65_536)
            .and_then(|_| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(Value::Null);
        let mut clients = BTreeMap::new();
        for (provider, default_id, prefix) in [
            (
                SubscriptionProvider::Antigravity,
                ANTIGRAVITY_CLIENT,
                "DOUANE_ANTIGRAVITY",
            ),
            (SubscriptionProvider::Gemini, GEMINI_CLIENT, "DOUANE_GEMINI"),
        ] {
            let config = file.get(provider.as_str()).unwrap_or(&Value::Null);
            let id = std::env::var(format!("{prefix}_CLIENT_ID"))
                .ok()
                .filter(|value| !value.trim().is_empty())
                .or_else(|| text(config, &["client_id"]))
                .unwrap_or_else(|| default_id.into());
            let secret = std::env::var(format!("{prefix}_CLIENT_SECRET"))
                .ok()
                .filter(|value| !value.trim().is_empty())
                .or_else(|| text(config, &["client_secret"]));
            if let Some(secret) = secret {
                clients.insert(
                    provider,
                    ClientMetadata {
                        id,
                        secret,
                        project: std::env::var(format!("{prefix}_PROJECT_ID"))
                            .ok()
                            .filter(|value| !value.trim().is_empty())
                            .or_else(|| text(config, &["project_id"])),
                    },
                );
            }
        }
        Self {
            client,
            clients,
            token_url: "https://oauth2.googleapis.com/token".into(),
            userinfo_url: "https://www.googleapis.com/oauth2/v2/userinfo?alt=json".into(),
            codeassist_url: "https://cloudcode-pa.googleapis.com/v1internal".into(),
            daily_url: "https://daily-cloudcode-pa.googleapis.com/v1internal".into(),
            poll_delay: Duration::from_secs(5),
        }
    }

    pub(crate) fn supports(&self, provider: SubscriptionProvider) -> bool {
        self.clients.contains_key(&provider)
    }

    pub(crate) fn authorize(
        &self,
        provider: SubscriptionProvider,
        state: &str,
        redirect: &str,
        challenge: &str,
    ) -> Result<String, SubscriptionError> {
        let metadata = self
            .clients
            .get(&provider)
            .ok_or(SubscriptionError::OAuthNotConfigured)?;
        let mut scopes = vec![
            "https://www.googleapis.com/auth/cloud-platform",
            "https://www.googleapis.com/auth/userinfo.email",
            "https://www.googleapis.com/auth/userinfo.profile",
        ];
        if provider == SubscriptionProvider::Antigravity {
            scopes.extend([
                "https://www.googleapis.com/auth/cclog",
                "https://www.googleapis.com/auth/experimentsandconfigs",
            ]);
        }
        let scopes = scopes.join(" ");
        let mut url = reqwest::Url::parse("https://accounts.google.com/o/oauth2/v2/auth")
            .map_err(|_| SubscriptionError::InvalidResponse)?;
        url.query_pairs_mut().extend_pairs([
            ("client_id", metadata.id.as_str()),
            ("redirect_uri", redirect),
            ("response_type", "code"),
            ("access_type", "offline"),
            ("prompt", "consent"),
            ("scope", scopes.as_str()),
            ("state", state),
        ]);
        if provider == SubscriptionProvider::Gemini
            && redirect == "https://codeassist.google.com/authcode"
        {
            url.query_pairs_mut().extend_pairs([
                ("code_challenge", challenge),
                ("code_challenge_method", "S256"),
            ]);
        }
        Ok(url.into())
    }

    pub(crate) async fn exchange(
        &self,
        provider: SubscriptionProvider,
        code: &str,
        redirect: &str,
        verifier: &str,
    ) -> Result<Value, SubscriptionError> {
        let metadata = self
            .clients
            .get(&provider)
            .ok_or(SubscriptionError::OAuthNotConfigured)?;
        let mut form = vec![
            ("client_id", metadata.id.as_str()),
            ("client_secret", metadata.secret.as_str()),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect),
        ];
        if provider == SubscriptionProvider::Gemini
            && redirect == "https://codeassist.google.com/authcode"
        {
            form.push(("code_verifier", verifier));
        }
        let mut tokens = send_json(self.client.post(&self.token_url).form(&form)).await?;
        let access = text(&tokens, &["access_token"]).ok_or(SubscriptionError::InvalidResponse)?;
        let profile = send_json(self.client.get(&self.userinfo_url).bearer_auth(&access)).await?;
        let email = text(&profile, &["email"]).ok_or(SubscriptionError::InvalidResponse)?;
        let (project, tier) = self
            .onboard(provider, &access, metadata.project.as_deref())
            .await?;
        let expires = tokens
            .get("expires_in")
            .and_then(Value::as_i64)
            .filter(|seconds| *seconds > 0)
            .map(|seconds| {
                timestamp(
                    time::OffsetDateTime::now_utc()
                        + time::Duration::seconds(seconds.min(31_536_000)),
                )
            });
        let object = tokens
            .as_object_mut()
            .ok_or(SubscriptionError::InvalidResponse)?;
        object.insert("type".into(), json!(provider.as_str()));
        object.insert("email".into(), json!(email));
        object.insert(
            "account_id".into(),
            profile.get("id").cloned().unwrap_or_else(|| json!(email)),
        );
        object.insert("project_id".into(), json!(project));
        object.insert("client_id".into(), json!(metadata.id));
        if let Some(tier) = tier {
            object.insert("plan".into(), json!(tier));
        }
        if let Some(expires) = expires {
            object.insert("expired".into(), json!(expires));
        }
        Ok(tokens)
    }

    pub(crate) fn refresh_request(
        &self,
        provider: SubscriptionProvider,
        refresh: &str,
        imported_id: Option<&str>,
        imported_secret: Option<&str>,
    ) -> Result<reqwest::RequestBuilder, SubscriptionError> {
        let metadata = self.clients.get(&provider);
        let id = imported_id
            .or_else(|| metadata.map(|metadata| metadata.id.as_str()))
            .ok_or(SubscriptionError::OAuthNotConfigured)?;
        let secret = imported_secret
            .or_else(|| metadata.map(|metadata| metadata.secret.as_str()))
            .ok_or(SubscriptionError::OAuthNotConfigured)?;
        Ok(self.client.post(&self.token_url).form(&[
            ("client_id", id),
            ("client_secret", secret),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
        ]))
    }

    async fn onboard(
        &self,
        provider: SubscriptionProvider,
        access: &str,
        project: Option<&str>,
    ) -> Result<(String, Option<String>), SubscriptionError> {
        let metadata = if provider == SubscriptionProvider::Antigravity {
            json!({"ideType":"ANTIGRAVITY"})
        } else {
            json!({"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"})
        };
        let mut body = json!({"metadata":metadata});
        if let Some(project) = project {
            body["cloudaicompanionProject"] = json!(project);
        }
        let mut request = self
            .client
            .post(format!("{}:loadCodeAssist", self.codeassist_url))
            .bearer_auth(access)
            .json(&body);
        if provider == SubscriptionProvider::Antigravity {
            request = request.header("User-Agent", "antigravity/hub/2.9.1");
        }
        let loaded = send_json(request).await?;
        let tier = loaded
            .get("paidTier")
            .or_else(|| loaded.get("currentTier"))
            .and_then(|tier| text(tier, &["id"]));
        if let Some(project) = project_id(&loaded) {
            return Ok((project, tier));
        }
        if provider == SubscriptionProvider::Gemini
            && loaded.get("currentTier").is_some_and(Value::is_object)
        {
            return project.map(|project| (project.to_owned(), tier)).ok_or(
                SubscriptionError::InvalidImport(
                    "Google account requires a configured Cloud project",
                ),
            );
        }
        let selected = loaded
            .get("allowedTiers")
            .and_then(Value::as_array)
            .and_then(|tiers| tiers.iter().find(|tier| tier["isDefault"] == true))
            .and_then(|tier| text(tier, &["id"]))
            .or(tier)
            .unwrap_or_else(|| {
                if provider == SubscriptionProvider::Antigravity {
                    "free-tier".into()
                } else {
                    "legacy-tier".into()
                }
            });
        if provider == SubscriptionProvider::Gemini && selected != "free-tier" && project.is_none()
        {
            return Err(SubscriptionError::InvalidImport(
                "Google account requires a configured Cloud project",
            ));
        }
        let (endpoint, body) = if provider == SubscriptionProvider::Antigravity {
            (
                format!("{}:onboardUser", self.daily_url),
                json!({"tier_id":selected,"metadata":{"ide_type":"ANTIGRAVITY","ide_name":"antigravity","ide_version":"2.9.1"}}),
            )
        } else {
            let mut body = json!({"tierId":selected,"metadata":metadata});
            if selected != "free-tier" {
                body["cloudaicompanionProject"] = json!(project);
                body["metadata"]["duetProject"] = json!(project);
            }
            (format!("{}:onboardUser", self.codeassist_url), body)
        };
        let onboard_request = || {
            let mut request = self.client.post(&endpoint).bearer_auth(access).json(&body);
            if provider == SubscriptionProvider::Antigravity {
                request = request
                    .header(
                        "User-Agent",
                        "antigravity/hub/2.9.1 google-api-nodejs-client/10.3.0",
                    )
                    .header("X-Goog-Api-Client", "gl-node/22.21.1");
            }
            request
        };
        let mut operation = send_json(onboard_request()).await?;
        for attempt in 0..12 {
            if operation["done"] == true {
                let project = operation
                    .get("response")
                    .and_then(project_id)
                    .ok_or(SubscriptionError::InvalidResponse)?;
                return Ok((project, Some(selected)));
            }
            if attempt == 11 {
                break;
            }
            tokio::time::sleep(self.poll_delay).await;
            if provider == SubscriptionProvider::Antigravity {
                operation = send_json(onboard_request()).await?;
            } else {
                let name = text(&operation, &["name"]).ok_or(SubscriptionError::InvalidResponse)?;
                if !name.starts_with("operations/")
                    || name.contains("..")
                    || name.contains('?')
                    || name.contains('#')
                {
                    return Err(SubscriptionError::InvalidResponse);
                }
                operation = send_json(
                    self.client
                        .get(format!("{}/{name}", self.codeassist_url))
                        .bearer_auth(access),
                )
                .await?;
            }
        }
        Err(SubscriptionError::InvalidImport(
            "Google account onboarding did not finish",
        ))
    }
}

fn project_id(value: &Value) -> Option<String> {
    ["cloudaicompanionProject", "projectId", "project"]
        .into_iter()
        .find_map(|key| {
            value.get(key).and_then(|project| {
                project
                    .as_str()
                    .filter(|project| !project.trim().is_empty())
                    .map(str::to_owned)
                    .or_else(|| text(project, &["id"]))
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[tokio::test]
    async fn google_code_exchange_profiles_and_onboards_a_managed_project() {
        let polls = Arc::new(AtomicUsize::new(0));
        let worker_polls = polls.clone();
        let app = axum::Router::new()
            .route("/token",axum::routing::post(|axum::Form(form): axum::Form<BTreeMap<String,String>>|async move{
                assert_eq!(form["grant_type"],"authorization_code");assert_eq!(form["client_id"],"fixture-client");
                assert_eq!(form["redirect_uri"],"https://codeassist.google.com/authcode");assert_eq!(form["code_verifier"],"fixture-verifier");
                axum::Json(json!({"access_token":"fixture-access","refresh_token":"fixture-refresh","expires_in":3600}))}))
            .route("/userinfo",axum::routing::get(||async{axum::Json(json!({"email":"fixture@example.test","id":"google-subject"}))}))
            .route("/v1internal:loadCodeAssist",axum::routing::post(||async{axum::Json(json!({"allowedTiers":[{"id":"free-tier","isDefault":true}]}))}))
            .route("/v1internal:onboardUser",axum::routing::post(|axum::Json(body):axum::Json<Value>|async move{
                assert_eq!(body["tierId"],"free-tier");assert!(body.get("cloudaicompanionProject").is_none());
                assert_eq!(body["metadata"]["pluginType"],"GEMINI");
                axum::Json(json!({"name":"operations/fixture","done":false}))}))
            .route("/v1internal/operations/fixture",axum::routing::get(move||{let polls=worker_polls.clone();async move{
                polls.fetch_add(1,Ordering::SeqCst);axum::Json(json!({"done":true,"response":{"cloudaicompanionProject":{"id":"managed-project"}}}))
            }}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        let mut google = GoogleOAuth::load(
            reqwest::Client::new(),
            &directory.path().join("absent.json"),
        );
        google.clients.insert(
            SubscriptionProvider::Gemini,
            ClientMetadata {
                id: "fixture-client".into(),
                secret: "fixture-public-client-metadata".into(),
                project: None,
            },
        );
        google.token_url = format!("{endpoint}/token");
        google.userinfo_url = format!("{endpoint}/userinfo");
        google.codeassist_url = format!("{endpoint}/v1internal");
        google.poll_delay = Duration::ZERO;
        let value = google
            .exchange(
                SubscriptionProvider::Gemini,
                "fixture-code",
                "https://codeassist.google.com/authcode",
                "fixture-verifier",
            )
            .await
            .unwrap();
        assert_eq!(value["project_id"], "managed-project");
        assert_eq!(value["email"], "fixture@example.test");
        assert_eq!(value["type"], "gemini");
        assert!(value.get("client_secret").is_none());
        assert_eq!(polls.load(Ordering::SeqCst), 1);
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn antigravity_onboards_and_refreshes_with_runtime_client_metadata() {
        let polls = Arc::new(AtomicUsize::new(0));
        let worker_polls = polls.clone();
        let app = axum::Router::new()
            .route("/token", axum::routing::post(|axum::Form(form):axum::Form<BTreeMap<String,String>>|async move {
                assert_eq!(form["grant_type"],"refresh_token");assert_eq!(form["client_id"],"fixture-antigravity");
                assert_eq!(form["refresh_token"],"fixture-refresh");
                axum::Json(json!({"access_token":"fixture-rotated","expires_in":3600}))
            }))
            .route("/v1internal:loadCodeAssist",axum::routing::post(|axum::Json(body):axum::Json<Value>|async move {
                assert_eq!(body["metadata"]["ideType"],"ANTIGRAVITY");
                axum::Json(json!({"allowedTiers":[{"id":"free-tier","isDefault":true}]}))
            }))
            .route("/daily:onboardUser",axum::routing::post(move|headers:axum::http::HeaderMap, axum::Json(body):axum::Json<Value>|{
                let polls=worker_polls.clone();async move {
                    assert_eq!(body["tier_id"],"free-tier");assert_eq!(body["metadata"]["ide_type"],"ANTIGRAVITY");
                    assert_eq!(headers["x-goog-api-client"],"gl-node/22.21.1");
                    let ready = polls.fetch_add(1,Ordering::SeqCst) > 0;
                    axum::Json(if ready {json!({"done":true,"response":{"cloudaicompanionProject":"antigravity-project"}})}else{json!({"done":false})})
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        let mut google = GoogleOAuth::load(
            reqwest::Client::new(),
            &directory.path().join("absent.json"),
        );
        google.clients.insert(
            SubscriptionProvider::Antigravity,
            ClientMetadata {
                id: "fixture-antigravity".into(),
                secret: "fixture-public-metadata".into(),
                project: None,
            },
        );
        google.codeassist_url = format!("{endpoint}/v1internal");
        google.daily_url = format!("{endpoint}/daily");
        google.token_url = format!("{endpoint}/token");
        google.poll_delay = Duration::ZERO;
        let (project, tier) = google
            .onboard(SubscriptionProvider::Antigravity, "fixture-access", None)
            .await
            .unwrap();
        assert_eq!(project, "antigravity-project");
        assert_eq!(tier.as_deref(), Some("free-tier"));
        assert_eq!(polls.load(Ordering::SeqCst), 2);
        let tokens = send_json(
            google
                .refresh_request(
                    SubscriptionProvider::Antigravity,
                    "fixture-refresh",
                    Some("fixture-antigravity"),
                    None,
                )
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(tokens["access_token"], "fixture-rotated");
        task.abort();
        let _ = task.await;
    }

    #[test]
    fn no_config_means_no_advertised_google_login_and_oob_has_pkce() {
        let directory = tempfile::tempdir().unwrap();
        let mut google = GoogleOAuth::load(
            reqwest::Client::new(),
            &directory.path().join("absent.json"),
        );
        google.clients.clear();
        assert!(!google.supports(SubscriptionProvider::Antigravity));
        google.clients.insert(
            SubscriptionProvider::Gemini,
            ClientMetadata {
                id: "fixture-client".into(),
                secret: "fixture-metadata".into(),
                project: None,
            },
        );
        let url = google
            .authorize(
                SubscriptionProvider::Gemini,
                "state",
                "https://codeassist.google.com/authcode",
                "challenge",
            )
            .unwrap();
        assert!(url.contains("code_challenge_method=S256"));
        assert!(!url.contains("fixture-metadata"));
    }
}
