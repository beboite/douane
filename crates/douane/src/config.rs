//! The broker's configuration.
//!
//! Nothing in here is a secret. Every credential is a [`SecretRef`] pointing
//! into the vault, so the file is safe to read, diff, and commit to a private
//! repo. That property is load-bearing: the moment a real key can appear in
//! this file, someone will paste one in.

use std::collections::BTreeMap;
use std::path::Path;

use douane_vault::SecretRef;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub server: Server,
    /// Upstreams douane can forward to, by name.
    pub providers: BTreeMap<String, Provider>,
    /// What a requested model name resolves to.
    pub routes: BTreeMap<String, Route>,
    /// Who may call, and what they are allowed to reach.
    pub clients: BTreeMap<String, Client>,
    #[serde(default)]
    pub scrub: ScrubConfig,
    /// The outbound proxy. Off unless it names a port to listen on: it is only
    /// useful once its authority is installed on the client, and nothing that
    /// needs that should switch itself on.
    #[serde(default)]
    pub egress: crate::egress::EgressConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Server {
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Where the request log goes. One SQLite file, append-mostly.
    #[serde(default = "default_db")]
    pub database: String,
    /// Vault entry holding the dashboard's token. Absent means no dashboard:
    /// an admin surface with no credential to guard it does not get served at
    /// all, rather than served openly.
    #[serde(default)]
    pub admin_key: Option<SecretRef>,
    /// Where to POST a one-line notification when an agent files a credential
    /// request (an ntfy topic URL, typically). Fire and forget: a notification
    /// that cannot be delivered never blocks or fails the request itself.
    /// Absent means the dashboard's pending list is the only signal.
    #[serde(default)]
    pub notify_url: Option<String>,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            database: default_db(),
            admin_key: None,
            notify_url: None,
        }
    }
}

fn default_listen() -> String {
    // Loopback by default. Exposing the broker on a LAN interface is a decision
    // that must be typed out, not inherited from a default.
    "127.0.0.1:8787".to_owned()
}

fn default_db() -> String {
    "douane.sqlite".to_owned()
}

/// How much a given upstream is trusted with.
///
/// This is the axis the whole project turns on. It is not about how good the
/// model is, it is about who reads the conversation afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Trust {
    /// Runs on hardware the user owns. Nothing leaves the LAN.
    Max,
    /// A vendor with a contract and a reputation to lose.
    Mid,
    /// A reseller. Assume every byte sent is stored and read.
    Low,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Provider {
    /// Base URL, no trailing slash. `https://api.deepseek.com/anthropic`.
    pub base_url: String,
    /// Which vault entry holds this provider's real key.
    #[serde(default)]
    pub key: Option<SecretRef>,
    /// Header the key goes into. Anthropic-compatible upstreams want
    /// `x-api-key`; OpenAI-compatible ones want `authorization` with a
    /// `Bearer ` prefix, which [`Provider::header_value`] adds.
    #[serde(default = "default_auth_header")]
    pub auth_header: String,
    #[serde(default)]
    pub bearer: bool,
    /// Which dialect this upstream speaks. `openai` makes douane translate the
    /// request on the way out and the answer on the way back, so an agent still
    /// only ever writes and reads the Anthropic shape. See [`crate::wire`] for
    /// what survives that translation and what is refused rather than guessed.
    #[serde(default)]
    pub wire: crate::wire::Wire,
    pub trust: Trust,
    /// Seconds before giving up on the upstream. Streaming responses are exempt
    /// once the first byte has arrived.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// What a route generated from this provider's catalogue is called:
    /// `<prefix><upstream id>`. Three resellers sell the same model, so a bare
    /// model name could only ever reach one of them, and the prefix is what
    /// keeps `opus-5` on this site distinguishable from `opus-5` on the next.
    /// Defaults to the provider's own name followed by a dash.
    #[serde(default)]
    pub prefix: Option<String>,
    /// Where this upstream publishes what it sells. Absent means the dialect's
    /// usual path ([`crate::wire::Wire::models_path`]); an absolute URL is used
    /// as written, for the sites that publish a catalogue somewhere else
    /// entirely. `catalog = false` on the provider turns the listing off for an
    /// upstream that has none, and its models are then added by hand.
    #[serde(default)]
    pub models_path: Option<String>,
    #[serde(default = "yes")]
    pub catalog: bool,
    /// Where this upstream says which dialect each of its models speaks. The
    /// new-api family, which is most of these resellers, publishes it on a
    /// public price page that needs no credential, and it is the only place
    /// that fact exists: one account can be sold on two surfaces, both listing
    /// the same ids, while a given id answers on exactly one of them.
    ///
    /// Relative to the host, not to `base_url`, because a provider whose
    /// `base_url` carries a version segment publishes this page at the root
    /// anyway. An empty string turns the lookup off.
    #[serde(default)]
    pub dialects_path: Option<String>,
}

/// Where the new-api family publishes its model table. Tried once per refresh,
/// without a credential; a 404 simply means douane learns nothing and serves
/// everything the listing named.
pub const DEFAULT_DIALECTS_PATH: &str = "/api/pricing";

fn default_auth_header() -> String {
    "x-api-key".to_owned()
}

fn default_timeout() -> u64 {
    120
}

impl Provider {
    pub fn header_value(&self, key: &str) -> String {
        if self.bearer {
            format!("Bearer {key}")
        } else {
            key.to_owned()
        }
    }

    /// The full URL of this provider's catalogue, or `None` when it has none.
    pub fn catalog_url(&self) -> Option<String> {
        if !self.catalog {
            return None;
        }
        Some(match self.models_path.as_deref() {
            Some(p) if p.starts_with("http://") || p.starts_with("https://") => p.to_owned(),
            Some(p) => format!("{}{}", self.base_url.trim_end_matches('/'), p),
            None => format!(
                "{}{}",
                self.base_url.trim_end_matches('/'),
                self.wire.models_path()
            ),
        })
    }

    /// The full URL of the page saying which dialect each model speaks, or
    /// `None` when this provider has none.
    pub fn dialects_url(&self) -> Option<String> {
        if !self.catalog {
            return None;
        }
        Some(match self.dialects_path.as_deref() {
            Some(p) if p.trim().is_empty() => return None,
            Some(p) if p.starts_with("http://") || p.starts_with("https://") => p.to_owned(),
            Some(p) => format!("{}{}", origin_of(&self.base_url), p),
            None => format!("{}{}", origin_of(&self.base_url), DEFAULT_DIALECTS_PATH),
        })
    }
}

/// The host a base URL points at. Two providers sharing one is one
/// subscription sold on two surfaces, which is worth saying on screen.
pub fn host_of(base_url: &str) -> String {
    let origin = origin_of(base_url);
    match origin.find("://") {
        Some(i) => origin[i + 3..].to_owned(),
        None => origin,
    }
}

/// Scheme and host of a base URL, with whatever path it carries dropped.
fn origin_of(base_url: &str) -> String {
    let url = base_url.trim_end_matches('/');
    match url.find("://") {
        Some(i) => match url[i + 3..].find('/') {
            Some(j) => url[..i + 3 + j].to_owned(),
            None => url.to_owned(),
        },
        None => url.to_owned(),
    }
}

/// What a discovered model is called on this side.
///
/// Route names are the broker's own vocabulary, so an upstream id that would
/// make a confusing or unusable one is rewritten rather than trusted: an id
/// travels in a URL path in the dashboard and in a `--model` argument on the
/// client, and neither survives a slash or a space.
pub fn route_name_for(prefix: &str, upstream_id: &str) -> String {
    // The id is slugged on its own and only then joined to the prefix. Doing
    // both in one pass lets a prefix ending in a separator meet an id starting
    // with one and produce `cx--weird`.
    let mut slug = String::with_capacity(upstream_id.len());
    let mut last_dash = false;
    for c in upstream_id.chars() {
        if c.is_ascii_alphanumeric() || c == '.' {
            slug.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    format!("{prefix}{}", slug.trim_matches('-'))
}

/// Whether a name may be used as a route. The same rule the generated form
/// obeys, applied to what a human types into the dashboard.
pub fn is_usable_route_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '[' | ']'))
}

/// What happens when a client asks for a given model name.
///
/// The indirection is the feature: an agent asks for `haiku`, and what answers
/// is whatever the routing table says today. Claude Code only ever emits three
/// model tiers, so this is how a cheap upstream gets to serve subagents without
/// the agent knowing or caring.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Route {
    pub provider: String,
    /// The model name to send upstream. Defaults to the one that was requested.
    #[serde(default)]
    pub model: Option<String>,
    /// Tried in order if the primary fails. Each entry names another route.
    #[serde(default)]
    pub fallback: Vec<String>,
    /// What to call this route on screen, and the window a harness should size
    /// itself from. Both travel to clients through `GET /v1/models`, which is
    /// the whole reason they exist here: a launcher that has to name a route
    /// nicely, or open a session at the right length, would otherwise keep its
    /// own table of exactly these two fields and drift from this file. A
    /// discovered model carries them in the catalogue instead.
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub context_length: Option<u64>,
}

/// A change to one route, made from the dashboard and stored in the database.
///
/// This is the whole point of routing by model name: an agent asks for `haiku`
/// for the length of a session, and what answers can change under it between
/// two turns without the session knowing.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RouteOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// `Some(None)` is not expressible here, so clearing a model back to "send
    /// whatever was requested" is done by deleting the override, not by setting
    /// this to null.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// A caller. One per machine, per project, or per agent profile.
///
/// The virtual key lives in the vault like everything else, so this file can be
/// read by anyone without handing them access.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Client {
    pub key: SecretRef,
    /// Route names this client may use. Empty means all of them.
    #[serde(default)]
    pub routes: Vec<String>,
    /// Refuse anything above this trust level. A client pinned to `max` cannot
    /// reach a reseller even if a route would have allowed it.
    #[serde(default)]
    pub max_trust: Option<Trust>,
    #[serde(default)]
    pub note: Option<String>,
}

/// What to do about something found on its way out.
///
/// Ordered so that the strongest verdict among several hits is `max()`, which
/// is the only combining rule that is ever right here: one blocking hit in a
/// body full of warnings still blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Allow,
    /// Let it through, record an alert. The dashboard is where it surfaces.
    Warn,
    /// Let it through with every vault value rewritten to its placeholder.
    /// The provider reads `__DOUANE_PROXY_PASSWORD__` where the credential
    /// was, the conversation stays usable, and the alert says what was hidden.
    /// Sits above `Warn` because it modifies what leaves, and below `Block`
    /// because something still leaves: one blocking hit outranks any number of
    /// substitutions.
    Substitute,
    /// Refuse the request. The agent gets a 403 naming what was found, never
    /// the text that was found.
    Block,
}

/// Which patterns count as credential-shaped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Shapes {
    Off,
    /// Vendor prefixes only: `sk-ant-`, `ghp_`, `AKIA`, a PEM header. A match is
    /// strong evidence, and ordinary source code does not produce one.
    Vendor,
    /// Adds JWTs and `password = "..."` assignments. Real recall, real noise:
    /// an agent reading test fixtures trips these constantly.
    Aggressive,
}

/// One action per trust level.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct ByTrust {
    pub low: Action,
    pub mid: Action,
    pub max: Action,
}

impl ByTrust {
    pub fn at(&self, trust: Trust) -> Action {
        match trust {
            Trust::Low => self.low,
            Trust::Mid => self.mid,
            Trust::Max => self.max,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct ScrubConfig {
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "default_shapes")]
    pub shapes: Shapes,
    /// A value that is verbatim in the vault. No false positive is possible, so
    /// the default blocks anywhere it would leave the house.
    #[serde(default = "default_vault_hit")]
    pub vault_hit: ByTrust,
    /// Something that merely looks like a credential. Warns by default, because
    /// blocking a coding agent over a fixture is its own kind of damage.
    #[serde(default = "default_shape_hit")]
    pub shape_hit: ByTrust,
}

fn yes() -> bool {
    true
}

fn default_shapes() -> Shapes {
    Shapes::Vendor
}

fn default_vault_hit() -> ByTrust {
    ByTrust {
        low: Action::Block,
        mid: Action::Block,
        // The local model is on hardware the user owns, so there is nothing to
        // protect the credential from and every scan would be pure latency.
        max: Action::Allow,
    }
}

fn default_shape_hit() -> ByTrust {
    ByTrust {
        low: Action::Warn,
        mid: Action::Allow,
        max: Action::Allow,
    }
}

impl Default for ScrubConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            shapes: default_shapes(),
            vault_hit: default_vault_hit(),
            shape_hit: default_shape_hit(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {0}: {1}")]
    Io(std::path::PathBuf, std::io::Error),
    #[error("{0} is not valid TOML: {1}")]
    Parse(std::path::PathBuf, toml::de::Error),
    #[error("route `{route}` names provider `{provider}`, which is not defined")]
    UnknownProvider { route: String, provider: String },
    #[error("route `{route}` falls back to `{fallback}`, which is not a route")]
    UnknownFallback { route: String, fallback: String },
    #[error("client `{client}` is allowed route `{route}`, which is not defined")]
    UnknownRoute { client: String, route: String },
    #[error("route `{0}` falls back to itself")]
    SelfFallback(String),
    #[error(
        "scrub.shape_hit.{0} is `substitute`, but a shape hit has no vault value \
         to substitute: use `warn` or `block`"
    )]
    ShapeSubstitute(&'static str),
    #[error("{0}")]
    Egress(String),
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let raw =
            std::fs::read_to_string(path).map_err(|e| ConfigError::Io(path.to_path_buf(), e))?;
        let config: Config =
            toml::from_str(&raw).map_err(|e| ConfigError::Parse(path.to_path_buf(), e))?;
        config.validate()?;
        Ok(config)
    }

    /// Catch the dangling names now rather than at three in the morning on the
    /// first request that happens to take a fallback nobody ever exercised.
    pub fn validate(&self) -> Result<(), ConfigError> {
        for (name, route) in &self.routes {
            if !self.providers.contains_key(&route.provider) {
                return Err(ConfigError::UnknownProvider {
                    route: name.clone(),
                    provider: route.provider.clone(),
                });
            }
            for fb in &route.fallback {
                if fb == name {
                    return Err(ConfigError::SelfFallback(name.clone()));
                }
                if !self.routes.contains_key(fb) {
                    return Err(ConfigError::UnknownFallback {
                        route: name.clone(),
                        fallback: fb.clone(),
                    });
                }
            }
        }
        for (name, client) in &self.clients {
            for r in &client.routes {
                if !self.routes.contains_key(r) {
                    return Err(ConfigError::UnknownRoute {
                        client: name.clone(),
                        route: r.clone(),
                    });
                }
            }
        }
        // Substituting needs a value to substitute *with*. A shape hit is a
        // pattern match on text douane does not hold, so there is nothing to
        // write in its place and the config is asking for the impossible.
        for (level, action) in [
            ("low", self.scrub.shape_hit.low),
            ("mid", self.scrub.shape_hit.mid),
            ("max", self.scrub.shape_hit.max),
        ] {
            if action == Action::Substitute {
                return Err(ConfigError::ShapeSubstitute(level));
            }
        }
        // Reported all at once rather than one per run: a rule file is edited
        // in batches, and finding the second mistake only after fixing the
        // first is its own small punishment.
        let egress = self.egress.validate();
        if !egress.is_empty() {
            return Err(ConfigError::Egress(egress.join("; ")));
        }
        Ok(())
    }

    /// A copy of this config with the dashboard's routing changes applied.
    ///
    /// Overrides live in the request log rather than being written back into
    /// the TOML. Serialising the config back would strip every comment out of a
    /// file whose comments are most of its value, and it would also mean the
    /// admin surface could rewrite the parts of the file it has no business
    /// touching. The file stays the base, the database holds the diff.
    pub fn with_overrides(&self, overrides: &BTreeMap<String, RouteOverride>) -> Self {
        let mut out = self.clone();
        for (name, o) in overrides {
            let Some(route) = out.routes.get_mut(name) else {
                continue;
            };
            if let Some(p) = &o.provider {
                route.provider = p.clone();
            }
            if o.model.is_some() {
                route.model = o.model.clone();
            }
        }
        out
    }

    /// Resolve a requested model to the chain of routes to try, primary first.
    ///
    /// Returns an empty vector for a model with no route, which the caller
    /// turns into a 404 rather than silently forwarding somewhere.
    pub fn chain(&self, model: &str) -> Vec<&str> {
        let Some((name, route)) = self.routes.get_key_value(model) else {
            return Vec::new();
        };
        let mut out = vec![name.as_str()];
        for fb in &route.fallback {
            if let Some((n, _)) = self.routes.get_key_value(fb) {
                out.push(n.as_str());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[server]
listen = "127.0.0.1:8787"

[providers.anthropic]
base_url = "https://api.anthropic.com"
key = { namespace = "anthropic", key = "api_key" }
trust = "mid"

[providers.deepseek]
base_url = "https://api.deepseek.com/anthropic"
key = { namespace = "deepseek", key = "api_key" }
trust = "low"

[providers.local]
base_url = "http://10.0.0.10:11434"
trust = "max"

[routes.opus]
provider = "anthropic"

[routes.haiku]
provider = "deepseek"
model = "deepseek-chat"
fallback = ["opus"]

[routes.qwopus]
provider = "local"

[clients.workstation]
key = { namespace = "douane", key = "vk_workstation" }

[clients.paranoid]
key = { namespace = "douane", key = "vk_paranoid" }
routes = ["qwopus"]
max_trust = "max"
"#;

    fn sample() -> Config {
        toml::from_str(SAMPLE).unwrap()
    }

    #[test]
    fn the_sample_is_coherent() {
        let c = sample();
        c.validate().unwrap();
        assert_eq!(c.providers.len(), 3);
        assert_eq!(c.providers["deepseek"].trust, Trust::Low);
        assert_eq!(c.routes["haiku"].model.as_deref(), Some("deepseek-chat"));
    }

    #[test]
    fn a_provider_with_no_key_is_allowed() {
        // The local model needs no credential, and requiring a dummy one would
        // put a fake secret in the vault for no reason.
        let c = sample();
        assert!(c.providers["local"].key.is_none());
    }

    #[test]
    fn chains_start_with_the_primary() {
        let c = sample();
        assert_eq!(c.chain("haiku"), vec!["haiku", "opus"]);
        assert_eq!(c.chain("opus"), vec!["opus"]);
        assert!(c.chain("nonexistent").is_empty());
    }

    #[test]
    fn dangling_names_are_caught_at_load() {
        let mut c = sample();
        c.routes.get_mut("haiku").unwrap().provider = "ghost".into();
        assert!(matches!(
            c.validate(),
            Err(ConfigError::UnknownProvider { .. })
        ));

        let mut c = sample();
        c.routes.get_mut("haiku").unwrap().fallback = vec!["haiku".into()];
        assert!(matches!(c.validate(), Err(ConfigError::SelfFallback(_))));

        let mut c = sample();
        c.clients.get_mut("paranoid").unwrap().routes = vec!["ghost".into()];
        assert!(matches!(
            c.validate(),
            Err(ConfigError::UnknownRoute { .. })
        ));
    }

    #[test]
    fn trust_orders_from_max_down() {
        // Ordering is what lets a client cap itself: `max_trust = "mid"` must
        // reject `low`, and the derive gives that for free only if the variants
        // stay in this order.
        assert!(Trust::Max < Trust::Mid);
        assert!(Trust::Mid < Trust::Low);
    }

    #[test]
    fn an_override_repoints_a_route_without_touching_the_file() {
        let c = sample();
        let mut overrides = BTreeMap::new();
        overrides.insert(
            "haiku".to_owned(),
            RouteOverride {
                provider: Some("anthropic".into()),
                model: Some("claude-haiku-4-5".into()),
            },
        );
        let live = c.with_overrides(&overrides);
        assert_eq!(live.routes["haiku"].provider, "anthropic");
        assert_eq!(
            live.routes["haiku"].model.as_deref(),
            Some("claude-haiku-4-5")
        );
        live.validate().unwrap();

        // The base is untouched, so restarting reverts to the file and the
        // override is a live decision rather than a silent rewrite.
        assert_eq!(c.routes["haiku"].provider, "deepseek");
    }

    #[test]
    fn an_override_naming_a_route_that_is_gone_is_ignored() {
        let c = sample();
        let mut overrides = BTreeMap::new();
        overrides.insert(
            "deleted".to_owned(),
            RouteOverride {
                provider: Some("anthropic".into()),
                model: None,
            },
        );
        let live = c.with_overrides(&overrides);
        assert!(!live.routes.contains_key("deleted"));
        live.validate().unwrap();
    }

    #[test]
    fn the_scrub_defaults_protect_the_reseller_path() {
        let c = sample();
        assert!(c.scrub.enabled);
        assert_eq!(c.scrub.vault_hit.at(Trust::Low), Action::Block);
        assert_eq!(c.scrub.vault_hit.at(Trust::Max), Action::Allow);
        assert_eq!(c.scrub.shape_hit.at(Trust::Low), Action::Warn);
        assert_eq!(c.scrub.shapes, Shapes::Vendor);
        // Combining verdicts takes the strongest.
        assert_eq!(Action::Warn.max(Action::Block), Action::Block);
        assert_eq!(Action::Allow.max(Action::Warn), Action::Warn);
    }

    #[test]
    fn substitute_sits_between_warn_and_block() {
        // It modifies what leaves, so it outranks a warning; something still
        // leaves, so one blocking hit outranks any number of substitutions.
        assert!(Action::Warn < Action::Substitute);
        assert!(Action::Substitute < Action::Block);
        assert_eq!(Action::Substitute.max(Action::Block), Action::Block);
        assert_eq!(Action::Warn.max(Action::Substitute), Action::Substitute);
    }

    #[test]
    fn a_shape_hit_cannot_ask_for_substitution() {
        // A shape hit is a pattern match on text douane does not hold, so
        // there is no value to substitute with and the config is nonsense.
        let mut c = sample();
        c.scrub.shape_hit.low = Action::Substitute;
        assert!(matches!(
            c.validate(),
            Err(ConfigError::ShapeSubstitute("low"))
        ));
    }

    #[test]
    fn bearer_providers_get_the_prefix() {
        let mut p = sample().providers["deepseek"].clone();
        assert_eq!(p.header_value("k"), "k");
        p.bearer = true;
        assert_eq!(p.header_value("k"), "Bearer k");
    }
}
