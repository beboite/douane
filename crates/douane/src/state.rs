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

/// The bundled catalog, each provider's models in that provider's own order.
pub(crate) fn catalog() -> &'static [Model] {
    static CATALOG: std::sync::OnceLock<Vec<Model>> = std::sync::OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("../data/models.json")).unwrap_or_default()
    })
}

/// A model's place in its provider's own list, the provider's default first.
/// A model the catalog does not name comes after the ones it does.
pub(crate) fn catalog_rank(provider: SubscriptionProvider, upstream_id: &str) -> usize {
    catalog()
        .iter()
        .filter(|model| model.provider == provider)
        .position(|model| model.id == upstream_id)
        .unwrap_or(usize::MAX)
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
    /// Last background quota refresh started for an account near its reserve.
    reserve_refresh: Mutex<BTreeMap<String, std::time::Instant>>,
}

impl AppState {
    pub fn new(mut config: Config, config_path: Option<PathBuf>) -> Result<Arc<Self>> {
        config.settings.validate()?;
        config.server.normalize()?;
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
            reserve_refresh: Mutex::new(BTreeMap::new()),
        }))
    }

    pub async fn refresh_models(&self) {
        let bundled = catalog();
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
            let now = time::OffsetDateTime::now_utc();
            let mut eligible = vec![];
            let mut candidates = vec![];
            for a in self.accounts.eligible(provider, Some(model)).await {
                let quota = self.quotas.get(&a.id).await;
                if self.reserve_excludes_with(&a, quota.as_ref(), now) {
                    continue;
                }
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
                    credits_only: quota.as_ref().is_some_and(|q| q.credits_only(now)),
                });
                eligible.push(a);
            }
            let strategy = config
                .settings
                .provider_strategies
                .get(provider.as_str())
                .copied()
                .unwrap_or(config.settings.strategy);
            let now = now.unix_timestamp();
            if let Some(key) = session {
                let pinned = self
                    .affinity
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(provider.as_str(), key);
                // A pin holds unless it would spend usage credits while another
                // account still has subscription quota, or reset-first has an
                // eligible account with an earlier weekly reset.
                let any_quota = candidates.iter().any(|c| !c.credits_only);
                if let Some(index) = pinned.and_then(|id| {
                    candidates.iter().position(|c| {
                        c.id == id
                            && (!c.credits_only || !any_quota)
                            && Scheduler::pin_holds(strategy, &candidates, &id, now)
                    })
                }) {
                    let mut pinned = eligible;
                    pinned.swap(0, index);
                    return Ok((model.into(), pinned));
                }
            }
            let order = self
                .scheduler
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .order(provider.as_str(), strategy, &candidates, now);
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

    /// The account would pay with usage credits although another eligible
    /// account of its provider still has subscription quota for `model`.
    pub async fn credits_avoidable(&self, account: &AccountCredential, model: &str) -> bool {
        let now = time::OffsetDateTime::now_utc();
        if !self
            .quotas
            .get(&account.id)
            .await
            .is_some_and(|q| q.credits_only(now))
        {
            return false;
        }
        for other in self.accounts.eligible(account.provider, Some(model)).await {
            if other.id != account.id
                && !self
                    .quotas
                    .get(&other.id)
                    .await
                    .is_some_and(|q| q.credits_only(now))
                && !self.reserve_reached(&other).await
            {
                return true;
            }
        }
        false
    }

    /// Douane leaves the account's reserve unused: an account-wide quota
    /// window at or below it takes the account out of rotation until that
    /// window resets. Unknown quotas never trigger it. This check only reads;
    /// request routing uses [`Self::reserve_excludes`].
    pub async fn reserve_reached(&self, account: &AccountCredential) -> bool {
        let snapshot = self.quotas.get(&account.id).await;
        reserve_reached_in(
            account.reserve(),
            snapshot.as_ref(),
            time::OffsetDateTime::now_utc(),
        )
    }

    /// Routing check for the reserve. Close to or at the reserve, a snapshot
    /// older than a minute also starts a background quota refresh, so the
    /// account leaves rotation in time and returns soon after its reset.
    pub async fn reserve_excludes(&self, account: &AccountCredential) -> bool {
        let snapshot = self.quotas.get(&account.id).await;
        self.reserve_excludes_with(account, snapshot.as_ref(), time::OffsetDateTime::now_utc())
    }

    fn reserve_excludes_with(
        &self,
        account: &AccountCredential,
        snapshot: Option<&crate::subscriptions::QuotaSnapshot>,
        now: time::OffsetDateTime,
    ) -> bool {
        let (Some(reserve), Some(snapshot)) = (account.reserve(), snapshot) else {
            return false;
        };
        let near = snapshot
            .lowest_account_window(now)
            .is_some_and(|lowest| lowest <= reserve + 10.0);
        let stale = crate::subscriptions::parse_timestamp(&snapshot.updated_at)
            .is_none_or(|at| now - at > time::Duration::minutes(1));
        if near && stale {
            self.refresh_soon(&account.id);
        }
        reserve_reached_in(Some(reserve), Some(snapshot), now)
    }

    fn refresh_soon(&self, id: &str) {
        let started = std::time::Instant::now();
        let throttle = std::time::Duration::from_secs(60);
        {
            let mut last = self
                .reserve_refresh
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            // Expired entries go, so deleted accounts do not accumulate.
            last.retain(|_, at| started.duration_since(*at) < throttle);
            if last.contains_key(id) {
                return;
            }
            last.insert(id.to_owned(), started);
        }
        let quotas = self.quotas.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            let _ = quotas.refresh(&id).await;
        });
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

/// A valid reserve and a snapshot whose account-wide window sits at or below
/// it. Shared by routing and the management listing.
pub fn reserve_reached_in(
    reserve: Option<f64>,
    snapshot: Option<&crate::subscriptions::QuotaSnapshot>,
    now: time::OffsetDateTime,
) -> bool {
    reserve
        .filter(|percent| crate::subscriptions::valid_reserve(*percent))
        .zip(snapshot)
        .is_some_and(|(reserve, snapshot)| snapshot.reserve_reached(reserve, now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::Strategy;
    use crate::subscriptions::{QuotaCredits, QuotaSnapshot, QuotaWindow};
    use serde_json::json;

    fn credits_only(id: &str) -> QuotaSnapshot {
        QuotaSnapshot {
            account_id: id.into(),
            provider: SubscriptionProvider::Codex,
            windows: vec![QuotaWindow {
                id: "weekly".into(),
                label: "Codex Weekly".into(),
                remaining_percent: Some(0.0),
                reset_at: Some("2099-01-01T00:00:00Z".into()),
                used: Some(100.0),
                limit: Some(100.0),
            }],
            updated_at: crate::subscriptions::now(),
            error: None,
            subscription: None,
            credits: vec![QuotaCredits {
                id: "codex-credits".into(),
                label: "Credits".into(),
                used: None,
                limit: None,
                remaining: Some(10.0),
                unit: "credits".into(),
                reset_at: None,
                unlimited: Some(false),
                enabled: Some(true),
            }],
            verification_required: false,
            verification_available: false,
        }
    }

    fn weekly(id: &str, reset: &str) -> QuotaSnapshot {
        QuotaSnapshot {
            windows: vec![QuotaWindow {
                id: "weekly".into(),
                label: "Codex Weekly".into(),
                remaining_percent: Some(60.0),
                reset_at: Some(reset.into()),
                used: Some(40.0),
                limit: Some(100.0),
            }],
            credits: vec![],
            ..credits_only(id)
        }
    }

    #[tokio::test]
    async fn reset_first_moves_a_pinned_session_to_the_nearest_reset() {
        let directory = tempfile::TempDir::new().unwrap();
        let mut config = Config::default();
        config.server.auth_dir = directory.path().join("auth");
        config.server.database = directory.path().join("requests.sqlite");
        let state = AppState::new(config, None).unwrap();
        let mut ids = vec![];
        for name in ["later", "soon"] {
            let account = state
                .accounts
                .import(
                    json!({"type":"codex","access_token":"fixture","account_id":name,"name":name}),
                    None,
                )
                .await
                .unwrap();
            ids.push(account.id);
        }
        let (later, soon) = (ids[0].clone(), ids[1].clone());
        state
            .quotas
            .insert_for_test(weekly(&later, "2099-01-08T00:00:00Z"))
            .await;
        state
            .quotas
            .insert_for_test(weekly(&soon, "2099-01-01T00:00:00Z"))
            .await;
        // A failover or an earlier strategy left the conversation on "later".
        state
            .affinity
            .lock()
            .unwrap()
            .bind("codex", "session", &later);
        async fn first(state: &AppState, strategy: Strategy) -> String {
            state.config.write().await.settings.strategy = strategy;
            let (_, accounts) = state
                .candidates("gpt-5.5", Some("codex"), Some("session"))
                .await
                .unwrap();
            accounts[0].id.clone()
        }
        for strategy in [Strategy::Smart, Strategy::RoundRobin, Strategy::FillFirst] {
            assert_eq!(first(&state, strategy).await, later, "{strategy:?}");
        }
        assert_eq!(first(&state, Strategy::ResetFirst).await, soon);

        // While the nearest reset is unavailable, the pin stays on "later".
        state.accounts.set_enabled(&soon, false).await.unwrap();
        assert_eq!(first(&state, Strategy::ResetFirst).await, later);
    }

    #[tokio::test]
    async fn a_session_leaves_usage_credits_only_for_subscription_quota() {
        let directory = tempfile::TempDir::new().unwrap();
        let mut config = Config::default();
        config.server.auth_dir = directory.path().join("auth");
        config.server.database = directory.path().join("requests.sqlite");
        let state = AppState::new(config, None).unwrap();
        let mut ids = vec![];
        for name in ["paid", "quota"] {
            let account = state
                .accounts
                .import(
                    json!({"type":"codex","access_token":"fixture","account_id":name,"name":name}),
                    None,
                )
                .await
                .unwrap();
            ids.push(account.id);
        }
        state.quotas.insert_for_test(credits_only(&ids[0])).await;
        state
            .affinity
            .lock()
            .unwrap()
            .bind("codex", "session", &ids[0]);
        let paid = state.accounts.get(&ids[0]).await.unwrap();
        async fn first(state: &AppState) -> String {
            let (_, accounts) = state
                .candidates("gpt-5.5", Some("codex"), Some("session"))
                .await
                .unwrap();
            accounts[0].id.clone()
        }
        assert_eq!(first(&state).await, ids[1]);
        assert!(state.credits_avoidable(&paid, "gpt-5.5").await);

        // Once no subscription quota remains, the pinned account pays.
        state.accounts.set_enabled(&ids[1], false).await.unwrap();
        assert_eq!(first(&state).await, ids[0]);
        assert!(!state.credits_avoidable(&paid, "gpt-5.5").await);
    }

    #[tokio::test]
    async fn claude_haiku_5_5_goes_to_a_claude_account() {
        let directory = tempfile::TempDir::new().unwrap();
        let mut config = Config::default();
        config.server.auth_dir = directory.path().join("auth");
        config.server.database = directory.path().join("requests.sqlite");
        let state = AppState::new(config, None).unwrap();
        let claude = state
            .accounts
            .import(json!({"type":"claude","access_token":"fixture"}), None)
            .await
            .unwrap();
        state.refresh_models().await;
        let (upstream, accounts) = state
            .candidates("claude-haiku-5-5", None, None)
            .await
            .unwrap();
        assert_eq!(upstream, "claude-haiku-5-5");
        assert_eq!(accounts[0].id, claude.id);
    }

    #[tokio::test]
    async fn an_account_at_its_reserve_leaves_rotation_and_releases_its_pin() {
        let directory = tempfile::TempDir::new().unwrap();
        let mut config = Config::default();
        config.server.auth_dir = directory.path().join("auth");
        config.server.database = directory.path().join("requests.sqlite");
        let state = AppState::new(config, None).unwrap();
        let mut ids = vec![];
        for name in ["kept", "spare"] {
            let account = state
                .accounts
                .import(
                    json!({"type":"codex","access_token":"fixture","account_id":name,"name":name}),
                    None,
                )
                .await
                .unwrap();
            ids.push(account.id);
        }
        let (kept, spare) = (ids[0].clone(), ids[1].clone());
        let mut low = weekly(&kept, "2099-01-01T00:00:00Z");
        low.windows[0].remaining_percent = Some(5.0);
        state.quotas.insert_for_test(low).await;
        state
            .quotas
            .insert_for_test(weekly(&spare, "2099-01-08T00:00:00Z"))
            .await;
        state
            .affinity
            .lock()
            .unwrap()
            .bind("codex", "session", &kept);
        async fn order(state: &AppState) -> Result<Vec<String>, String> {
            state
                .candidates("gpt-5.5", Some("codex"), Some("session"))
                .await
                .map(|(_, accounts)| accounts.into_iter().map(|a| a.id).collect())
        }
        // Without a reserve, 5% left is still spent.
        assert_eq!(order(&state).await.unwrap()[0], kept);

        state
            .accounts
            .update(&kept, None, None, Some(Some(5.0)))
            .await
            .unwrap();
        assert_eq!(order(&state).await.unwrap(), vec![spare.clone()]);
        let reserved = state.accounts.get(&kept).await.unwrap();
        assert!(state.reserve_reached(&reserved).await);

        // The reserve is a hard floor: no other account means no request.
        state.accounts.set_enabled(&spare, false).await.unwrap();
        assert!(order(&state).await.is_err());

        // Above the reserve, the account serves again.
        state
            .accounts
            .update(&kept, None, None, Some(Some(4.0)))
            .await
            .unwrap();
        assert_eq!(order(&state).await.unwrap(), vec![kept]);
    }

    #[tokio::test]
    async fn a_stale_reached_reserve_refreshes_and_a_bad_stored_value_is_ignored() {
        let directory = tempfile::TempDir::new().unwrap();
        let mut config = Config::default();
        config.server.auth_dir = directory.path().join("auth");
        config.server.database = directory.path().join("requests.sqlite");
        let state = AppState::new(config, None).unwrap();
        let account = state
            .accounts
            .import(
                json!({"type":"codex","access_token":"fixture","account_id":"low"}),
                None,
            )
            .await
            .unwrap();
        state
            .accounts
            .update(&account.id, None, None, Some(Some(5.0)))
            .await
            .unwrap();
        let mut low = weekly(&account.id, "2099-01-01T00:00:00Z");
        low.windows[0].remaining_percent = Some(2.0);
        low.updated_at = "2026-01-01T00:00:00Z".into();
        state.quotas.insert_for_test(low).await;
        let credential = state.accounts.get(&account.id).await.unwrap();
        // The listing check reads only.
        assert!(state.reserve_reached(&credential).await);
        assert!(state.reserve_refresh.lock().unwrap().is_empty());
        // Removing the account keeps the spawned refresh off the network.
        state.accounts.delete(&account.id).await.unwrap();
        assert!(state.reserve_excludes(&credential).await);
        assert!(
            state
                .reserve_refresh
                .lock()
                .unwrap()
                .contains_key(&account.id)
        );

        // A hand-edited file outside 0 < reserve < 100 never parks the account.
        let mut edited = credential.clone();
        for invalid in [150.0, f64::INFINITY, f64::NAN, -3.0] {
            edited.reserve_percent = Some(invalid);
            assert!(!state.reserve_reached(&edited).await, "{invalid}");
            assert_eq!(edited.summary().reserve_percent, None);
        }
    }
}
