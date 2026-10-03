use crate::{
    ledger::Ledger,
    scheduler::{Candidate, Scheduler},
    settings::Config,
    subscriptions::{
        AccountCredential, AccountStore, OAuthManager, QuotaService, ResetManager,
        SubscriptionProvider,
    },
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::RwLock;

#[derive(Clone, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub provider: SubscriptionProvider,
    pub name: String,
    #[serde(default)]
    pub context_length: Option<u64>,
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub upstream_id: String,
    #[serde(default)]
    pub thinking: Option<Value>,
}
fn yes() -> bool {
    true
}

pub struct AppState {
    pub config: RwLock<Config>,
    pub config_path: Option<PathBuf>,
    pub accounts: Arc<AccountStore>,
    pub oauth: Arc<OAuthManager>,
    pub quotas: Arc<QuotaService>,
    pub resets: Arc<ResetManager>,
    pub ledger: Arc<Ledger>,
    pub client: reqwest::Client,
    pub models: RwLock<Vec<Model>>,
    pub scheduler: Mutex<Scheduler>,
    pub affinity: Mutex<crate::affinity::Affinity>,
    pub management_token: Option<String>,
    pub config_write: tokio::sync::Mutex<()>,
}

impl AppState {
    pub fn new(config: Config, config_path: Option<PathBuf>) -> Result<Arc<Self>> {
        config.settings.validate()?;
        let management_token = config
            .server
            .management_token_env
            .as_ref()
            .map(|name| std::env::var(name))
            .transpose()?;
        let accounts = Arc::new(AccountStore::open(config.server.auth_dir.clone())?);
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .pool_idle_timeout(std::time::Duration::from_secs(120))
            .pool_max_idle_per_host(32)
            .tcp_nodelay(true)
            .user_agent(concat!("douane/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let oauth = Arc::new(OAuthManager::new(client.clone(), accounts.clone()));
        let quotas = Arc::new(QuotaService::with_oauth(
            client.clone(),
            accounts.clone(),
            oauth.clone(),
        ));
        let resets = Arc::new(ResetManager::new(accounts.clone(), oauth.clone())?);
        let ledger = Ledger::open(&config.server.database)?;
        Ok(Arc::new(Self {
            config: RwLock::new(config),
            config_path,
            accounts,
            oauth,
            quotas,
            resets,
            ledger,
            client,
            models: RwLock::new(vec![]),
            scheduler: Mutex::new(Scheduler::default()),
            affinity: Mutex::new(crate::affinity::Affinity::default()),
            management_token,
            config_write: tokio::sync::Mutex::new(()),
        }))
    }

    pub async fn refresh_models(&self) {
        let bundled: Vec<Model> =
            serde_json::from_str(include_str!("../data/models.json")).unwrap_or_default();
        let mut models: BTreeMap<String, Model> = BTreeMap::new();
        let disabled = self.config.read().await.disabled_models.clone();
        for summary in self.accounts.list().await {
            let Some(account) = self.accounts.get(&summary.id).await else {
                continue;
            };
            let mut available: Vec<Model> = if account.models.is_empty() {
                bundled
                    .iter()
                    .filter(|m| m.provider == account.provider)
                    .cloned()
                    .collect()
            } else {
                account
                    .models
                    .iter()
                    .map(|id| Model {
                        id: id.clone(),
                        provider: account.provider,
                        name: id.clone(),
                        context_length: None,
                        max_output_tokens: None,
                        enabled: true,
                        upstream_id: id.clone(),
                        thinking: None,
                    })
                    .collect()
            };
            if matches!(
                account.provider,
                SubscriptionProvider::OpencodeGo
                    | SubscriptionProvider::OpenaiCompatible
                    | SubscriptionProvider::Qwen
            ) {
                let base = account
                    .base_url
                    .as_deref()
                    .unwrap_or(match account.provider {
                        SubscriptionProvider::OpencodeGo => "https://opencode.ai/zen/go/v1",
                        SubscriptionProvider::Qwen => "https://portal.qwen.ai/v1",
                        _ => "",
                    });
                if !base.is_empty() {
                    let root = base.trim_end_matches('/');
                    let url = if root.ends_with("/v1") {
                        format!("{root}/models")
                    } else {
                        format!("{root}/v1/models")
                    };
                    if let Ok(response) = self
                        .client
                        .get(url)
                        .bearer_auth(&account.access_token)
                        .timeout(std::time::Duration::from_secs(15))
                        .send()
                        .await
                    {
                        if response.status().is_success() {
                            if let Ok(value) = response.json::<Value>().await {
                                if let Some(data) = value["data"].as_array() {
                                    available = data
                                        .iter()
                                        .filter_map(|m| {
                                            Some(Model {
                                                id: m["id"].as_str()?.into(),
                                                name: m["name"]
                                                    .as_str()
                                                    .or_else(|| m["id"].as_str())?
                                                    .into(),
                                                provider: account.provider,
                                                context_length: m["context_length"]
                                                    .as_u64()
                                                    .or_else(|| m["context_window"].as_u64()),
                                                max_output_tokens: None,
                                                enabled: true,
                                                upstream_id: String::new(),
                                                thinking: m.get("thinking").cloned(),
                                            })
                                        })
                                        .filter(|m| {
                                            account.models.is_empty()
                                                || account.models.contains(&m.id)
                                        })
                                        .collect();
                                }
                            }
                        }
                    }
                }
            }
            for mut model in available {
                model.upstream_id = model.id.clone();
                model.id = format!("{}/{}", model.provider.as_str(), model.upstream_id);
                model.enabled =
                    !disabled.contains(&model.id) && !disabled.contains(&model.upstream_id);
                models.insert(model.id.clone(), model);
            }
        }
        *self.models.write().await = models.into_values().collect();
    }

    pub async fn candidates(
        &self,
        requested: &str,
        provider_hint: Option<&str>,
        session: Option<&str>,
    ) -> Result<(String, Vec<AccountCredential>), String> {
        let config = self.config.read().await.clone();
        if config.disabled_models.contains(requested) {
            return Err("model is disabled".into());
        }
        let (prefix, model) = requested
            .split_once('/')
            .filter(|(p, _)| SubscriptionProvider::parse(p).is_some())
            .map(|(p, m)| (Some(p), m))
            .unwrap_or((None, requested));
        let explicit = prefix
            .or(provider_hint)
            .or(config.settings.default_provider.as_deref());
        let all_models = self.models.read().await;
        let providers: Vec<SubscriptionProvider> = if let Some(p) = explicit {
            vec![SubscriptionProvider::parse(p).ok_or("unknown provider")?]
        } else {
            let mut providers = vec![];
            for m in all_models
                .iter()
                .filter(|m| m.upstream_id == model && m.enabled)
            {
                if !providers.contains(&m.provider) {
                    providers.push(m.provider);
                }
            }
            // Native names select the matching native provider ahead of aggregators.
            providers.sort_by_key(|p| match (model, *p) {
                (m, SubscriptionProvider::Codex) if m.starts_with("gpt-") => 0,
                (m, SubscriptionProvider::Claude) if m.starts_with("claude-") => 0,
                (m, SubscriptionProvider::Xai) if m.starts_with("grok-") => 0,
                _ => 1,
            });
            providers
        };
        drop(all_models);
        let mut ordered = vec![];
        for provider in providers {
            if config
                .disabled_models
                .contains(&format!("{}/{model}", provider.as_str()))
            {
                continue;
            }
            let eligible = self.accounts.eligible(provider, Some(model)).await;
            if let Some(key) = session {
                let pinned = self
                    .affinity
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(provider.as_str(), key);
                if let Some(index) = pinned.and_then(|id| eligible.iter().position(|a| a.id == id))
                {
                    let mut pinned = eligible;
                    pinned.swap(0, index);
                    return Ok((model.into(), pinned));
                }
            }
            let mut candidates = vec![];
            for a in &eligible {
                let quota = self.quotas.get(&a.id).await;
                candidates.push(Candidate {
                    id: a.id.clone(),
                    remaining_percent: quota.as_ref().and_then(|q| {
                        q.weekly_remaining_percent()
                            .or_else(|| q.remaining_percent())
                    }),
                    weekly_reset: quota
                        .as_ref()
                        .and_then(|q| q.weekly_reset())
                        .and_then(crate::subscriptions::parse_timestamp)
                        .map(|t| t.unix_timestamp()),
                });
            }
            let strategy = config
                .settings
                .provider_strategies
                .get(provider.as_str())
                .copied()
                .unwrap_or(config.settings.strategy);
            let order = self
                .scheduler
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .order(
                    provider.as_str(),
                    strategy,
                    &candidates,
                    time::OffsetDateTime::now_utc().unix_timestamp(),
                );
            for id in order {
                if let Some(a) = eligible.iter().find(|a| a.id == id) {
                    ordered.push(a.clone());
                }
            }
            // Automatic fallback stays inside one provider's subscription pool.
            if !ordered.is_empty() {
                break;
            }
        }
        if ordered.is_empty() {
            Err("no available subscription for this model; connect an account or wait for its quota reset".into())
        } else {
            Ok((model.into(), ordered))
        }
    }

    pub async fn refresh_quotas(&self, id: Option<&str>) {
        use futures_util::{StreamExt, stream};
        let ids: Vec<String> = self
            .accounts
            .list()
            .await
            .into_iter()
            .filter(|a| id.is_none_or(|id| a.id == id))
            .map(|a| a.id)
            .collect();
        stream::iter(ids)
            .for_each_concurrent(4, |id| async move {
                let _ = self.quotas.refresh(&id).await;
            })
            .await;
    }
}
