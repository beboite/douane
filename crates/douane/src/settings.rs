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
}

impl Default for Server {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8787".into(),
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
        }
    }
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
        Ok(())
    }
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let content =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut config: Self = toml::from_str(&content).context("invalid configuration; douane 0.2 uses subscription accounts instead of vault/provider/client sections")?;
        config.settings.validate()?;
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
