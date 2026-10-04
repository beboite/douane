//! Runtime configuration. Subscription credentials live in the auth directory.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: Server,
    pub settings: Settings,
    pub disabled_models: BTreeSet<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Server {
    pub listen: String,
    /// Public client API address when the dashboard uses a different origin.
    pub client_api_base_url: Option<String>,
    pub auth_dir: PathBuf,
    pub database: PathBuf,
    /// Optional management token, loaded from this environment variable.
    pub management_token_env: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub strategy: crate::scheduler::Strategy,
    pub provider_strategies: BTreeMap<String, crate::scheduler::Strategy>,
    pub quota_refresh_seconds: u64,
    pub request_timeout_seconds: u64,
    pub default_provider: Option<String>,
    /// How `/v1/quotas` presents each provider's accounts. Absent means per account.
    pub quota_display: BTreeMap<String, QuotaDisplay>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuotaDisplay {
    #[default]
    Accounts,
    Average,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8787".into(),
            client_api_base_url: None,
            auth_dir: "auth".into(),
            database: "douane.sqlite".into(),
            management_token_env: None,
        }
    }
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            strategy: crate::scheduler::Strategy::Smart,
            provider_strategies: BTreeMap::new(),
            quota_refresh_seconds: 300,
            request_timeout_seconds: 180,
            default_provider: None,
            quota_display: BTreeMap::new(),
        }
    }
}
impl Server {
    pub fn normalize(&mut self) -> Result<()> {
        if let Some(base) = &self.client_api_base_url {
            self.client_api_base_url = Some(normalize_client_api_base_url(base)?);
        }
        Ok(())
    }
}

fn normalize_client_api_base_url(value: &str) -> Result<String> {
    let invalid = || {
        anyhow::anyhow!(
            "client_api_base_url must be an absolute HTTP(S) URL without credentials, query, fragment, whitespace or control characters"
        )
    };
    if value
        .chars()
        .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(invalid());
    }
    let (scheme, authority) = value.split_once("://").ok_or_else(invalid)?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(invalid());
    }
    if authority
        .split(['/', '?', '#'])
        .next()
        .is_some_and(|authority| authority.contains('@'))
    {
        return Err(invalid());
    }
    let mut url = reqwest::Url::parse(value).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    let path = url.path().trim_end_matches('/');
    let path = if path.ends_with("/v1") {
        path.to_owned()
    } else {
        format!("{path}/v1")
    };
    url.set_path(&path);
    Ok(url.into())
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        if !(30..=86400).contains(&self.quota_refresh_seconds) {
            bail!("quota_refresh_seconds must be between 30 and 86400");
        }
        if !(10..=1800).contains(&self.request_timeout_seconds) {
            bail!("request_timeout_seconds must be between 10 and 1800");
        }
        if let Some(p) = &self.default_provider {
            if crate::subscriptions::SubscriptionProvider::parse(p).is_none() {
                bail!("unknown default provider");
            }
        }
        if self.quota_display.keys().any(|p| {
            crate::subscriptions::SubscriptionProvider::parse(p).map(|p| p.as_str()) != Some(p)
        }) {
            bail!("unknown quota display provider");
        }
        Ok(())
    }

    pub fn quota_display(
        &self,
        provider: crate::subscriptions::SubscriptionProvider,
    ) -> QuotaDisplay {
        self.quota_display
            .get(provider.as_str())
            .copied()
            .unwrap_or_default()
    }
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let content =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut config: Self = toml::from_str(&content).context("invalid configuration; douane 0.2 uses subscription accounts instead of vault/provider/client sections")?;
        config.settings.validate()?;
        config.server.normalize()?;
        let parent = path.parent().unwrap_or(Path::new("."));
        if config.server.auth_dir.is_relative() {
            config.server.auth_dir = parent.join(&config.server.auth_dir);
        }
        if config.server.database.is_relative() {
            config.server.database = parent.join(&config.server.database);
        }
        Ok(config)
    }
    pub async fn save(&self, path: &Path) -> Result<()> {
        let data = toml::to_string_pretty(self)?;
        let temporary = path.with_extension("toml.tmp");
        tokio::fs::write(&temporary, data).await?;
        tokio::fs::rename(&temporary, path).await?;
        Ok(())
    }
}
