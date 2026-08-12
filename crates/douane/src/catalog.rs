//! What each upstream sells, and which of it douane is willing to serve.
//!
//! The routing table used to be hand-written twice: once here in the TOML, and
//! once again in every client that had to know which names existed. A new model
//! at a reseller meant editing both, and forgetting either produced the same
//! symptom, `no route for model`, from opposite causes.
//!
//! So douane asks each upstream what it sells (`/v1/models` for the Anthropic
//! dialect, `/models` for the OpenAI one, both answering the same envelope) and
//! keeps the answer in the database. Nothing found this way is served until it
//! is switched on: **discovery is not permission**. A model sitting in the
//! catalogue disabled is a fact about the upstream; a model switched on is a
//! decision by the owner, and only then does it become a route.
//!
//! Three properties this file must keep:
//!
//! - **The TOML always wins.** A generated route whose name collides with one in
//!   the file is refused rather than applied, so nothing an upstream publishes
//!   can quietly repoint a route that was written by hand.
//! - **A listing is not a call.** The catalogue request carries the provider's
//!   real credential, exactly like a probe, and its answer is passed through the
//!   scrubber before it is stored: some upstreams echo a key back inside an
//!   error page, and the catalogue must not become the one place a value lands.
//! - **What disappeared is remembered, not deleted.** A model that stops being
//!   listed is marked missing and keeps its row, because "the reseller dropped
//!   this yesterday" is exactly the thing worth seeing on the dashboard, and
//!   deleting the row would also throw away the decision to enable it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::config::{Config, Provider, Route, route_name_for};
use crate::proxy::AppState;

/// Long enough for a slow reseller, short enough that one dead upstream does
/// not hold up a sweep of ten.
const CATALOG_TIMEOUT_SECS: u64 = 20;

/// Where a row came from. The distinction is worth keeping visible: a manual
/// row is one nothing upstream confirms, so it survives every refresh and is
/// never marked missing, and a wrong one fails at call time instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Read from the upstream's own catalogue.
    Probed,
    /// Typed in by the owner, for a site that publishes no catalogue.
    Manual,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Probed => "probed",
            Self::Manual => "manual",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "manual" => Self::Manual,
            _ => Self::Probed,
        }
    }
}

/// One row of the catalogue: a model an upstream sells, and what douane does
/// about it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    /// The provider it belongs to, as named in the TOML.
    pub provider: String,
    /// The id to send upstream, exactly as the upstream spells it.
    pub upstream_id: String,
    /// The name douane serves it under. Unique across the whole table, because
    /// it is what a client asks for.
    pub route: String,
    /// What to call it on screen. The upstream's own `display_name` when it
    /// gives one.
    pub label: Option<String>,
    /// The window the upstream advertises, when it advertises one. Passed on to
    /// clients so a harness can size its context without a second table to
    /// maintain by hand.
    pub context_length: Option<u64>,
    /// Off until the owner says otherwise. This is the whole safety property of
    /// this module: discovery adds rows, never routes.
    pub enabled: bool,
    pub source: Source,
    /// False once a refresh no longer finds it upstream. Kept rather than
    /// deleted: a model that vanished is a thing to see, not to forget.
    pub present: bool,
    pub first_seen: String,
    pub last_seen: String,
    pub note: Option<String>,
}

impl ModelEntry {
    /// The route this entry would install, or `None` when it is switched off.
    pub fn as_route(&self) -> Option<(String, Route)> {
        self.enabled.then(|| {
            (
                self.route.clone(),
                Route {
                    provider: self.provider.clone(),
                    model: Some(self.upstream_id.clone()),
                    fallback: Vec::new(),
                },
            )
        })
    }
}

impl Config {
    /// The table with every enabled catalogue entry added as a route.
    ///
    /// **The file wins every collision.** A catalogue row whose route name is
    /// already in the TOML is dropped here and reported by
    /// [`Config::model_collisions`], never merged: an upstream that started
    /// publishing an id matching a hand-written route must not be able to
    /// repoint it. Same for a row naming a provider that no longer exists,
    /// which is what a `douane.toml` edit leaves behind.
    pub fn with_models(&self, models: &[ModelEntry]) -> Self {
        let mut out = self.clone();
        for m in models {
            if self.routes.contains_key(&m.route) || !self.providers.contains_key(&m.provider) {
                continue;
            }
            if let Some((name, route)) = m.as_route() {
                out.routes.insert(name, route);
            }
        }
        out
    }

    /// Enabled entries the merge above had to drop, with the reason. The
    /// dashboard shows these: a switch that is on while the route it names is
    /// served by the file is a lie worth surfacing.
    pub fn model_collisions(&self, models: &[ModelEntry]) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for m in models.iter().filter(|m| m.enabled) {
            if self.routes.contains_key(&m.route) {
                out.insert(
                    m.route.clone(),
                    format!("`{}` is already a route in douane.toml", m.route),
                );
            } else if !self.providers.contains_key(&m.provider) {
                out.insert(
                    m.route.clone(),
                    format!("provider `{}` is not in douane.toml", m.provider),
                );
            }
        }
        out
    }
}

/// What a provider's catalogue answered.
#[derive(Debug, Clone, Serialize)]
pub struct Discovered {
    pub upstream_id: String,
    pub label: Option<String>,
    pub context_length: Option<u64>,
}

/// The outcome of asking one provider what it sells.
#[derive(Debug, Serialize)]
pub struct RefreshResult {
    pub provider: String,
    pub ok: bool,
    /// How many ids the upstream listed. Zero with `ok` is a real answer: some
    /// tokens are scoped to nothing.
    pub found: usize,
    /// Rows this refresh created, so the dashboard can say what is new without
    /// diffing two listings itself.
    pub added: usize,
    /// Rows that were in the catalogue and are no longer listed upstream.
    pub missing: usize,
    pub status: Option<u16>,
    pub ms: u64,
    /// The upstream's own words on failure, truncated and scrubbed.
    pub detail: Option<String>,
}

/// Read a catalogue envelope. Both dialects answer `{"data": [...]}`; a few
/// sites answer a bare array or wrap it in `models`, so all three are accepted.
/// Anything else is a failure with the body quoted, never a silent empty list:
/// "this upstream lists nothing" and "douane could not read the answer" are
/// different facts and the dashboard has to be able to tell them apart.
pub fn parse_catalogue(body: &[u8]) -> Result<Vec<Discovered>, String> {
    let v: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("the catalogue is not JSON: {e}"))?;
    let items = v
        .get("data")
        .or_else(|| v.get("models"))
        .or(Some(&v))
        .and_then(|x| x.as_array())
        .ok_or_else(|| "the catalogue has no `data` array".to_owned())?;

    let mut out = Vec::with_capacity(items.len());
    for item in items {
        // A plain array of names is legal here too: a couple of small
        // deployments answer `["model-a", "model-b"]`.
        let id = match item {
            serde_json::Value::String(s) => s.clone(),
            _ => match item.get("id").and_then(|x| x.as_str()) {
                Some(s) => s.to_owned(),
                None => continue,
            },
        };
        if id.is_empty() || id.len() > 200 {
            continue;
        }
        let label = item
            .get("display_name")
            .or_else(|| item.get("name"))
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty() && *s != id)
            .map(|s| s.chars().take(120).collect());
        let context_length = ["context_length", "context_window", "max_context_length"]
            .iter()
            .find_map(|k| item.get(*k).and_then(|x| x.as_u64()))
            .filter(|n| *n > 0);
        out.push(Discovered {
            upstream_id: id,
            label,
            context_length,
        });
    }
    Ok(out)
}

/// Ask one provider what it sells.
///
/// The request carries the provider's real credential, like a probe does, and
/// for the same reason: a catalogue is usually scoped to the token, so asking
/// without one answers about somebody else's account. What comes back goes
/// through the scrubber before any of it is stored or shown.
pub async fn fetch(state: &AppState, name: &str, provider: &Provider) -> RefreshResult {
    let started = std::time::Instant::now();
    let mut out = RefreshResult {
        provider: name.to_owned(),
        ok: false,
        found: 0,
        added: 0,
        missing: 0,
        status: None,
        ms: 0,
        detail: None,
    };

    let Some(url) = provider.catalog_url() else {
        out.detail = Some("this provider publishes no catalogue: add its models by hand".to_owned());
        out.ms = started.elapsed().as_millis() as u64;
        return out;
    };

    let mut req = state
        .http
        .get(&url)
        .header("anthropic-version", "2023-06-01")
        // The shape of real traffic, for the same reason the probe sends it:
        // agentrouter refuses a request that does not look like Claude Code,
        // on a perfectly valid key.
        .header("user-agent", "claude-cli/2.1.220 (external, cli)")
        .header("x-app", "cli")
        .timeout(std::time::Duration::from_secs(
            provider.timeout_secs.min(CATALOG_TIMEOUT_SECS),
        ));

    if let Some(key_ref) = &provider.key {
        let Some(key) = state.secret(key_ref) else {
            out.detail = Some(format!("the vault has no `{key_ref}`"));
            out.ms = started.elapsed().as_millis() as u64;
            return out;
        };
        let value = provider.header_value(key.expose());
        match axum::http::HeaderValue::from_str(&value) {
            Ok(v) => req = req.header(provider.auth_header.as_str(), v),
            Err(_) => {
                out.detail = Some("the key does not fit in a header".to_owned());
                out.ms = started.elapsed().as_millis() as u64;
                return out;
            }
        }
    }

    let response = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            let mut parts = vec![e.to_string()];
            let mut src = std::error::Error::source(&e);
            while let Some(s) = src {
                parts.push(s.to_string());
                src = s.source();
            }
            out.detail = Some(parts.join(": "));
            out.ms = started.elapsed().as_millis() as u64;
            return out;
        }
    };

    out.status = Some(response.status().as_u16());
    let success = response.status().is_success();
    let body = response.bytes().await.unwrap_or_default();

    if !success {
        out.detail = Some(short_and_clean(state, &body));
        out.ms = started.elapsed().as_millis() as u64;
        return out;
    }

    let found = match parse_catalogue(&body) {
        Ok(f) => f,
        Err(e) => {
            out.detail = Some(format!("{e} ({})", short_and_clean(state, &body)));
            out.ms = started.elapsed().as_millis() as u64;
            return out;
        }
    };

    out.ok = true;
    out.found = found.len();

    let prefix = provider
        .prefix
        .clone()
        .unwrap_or_else(|| format!("{name}-"));
    let rows: Vec<(String, Discovered)> = found
        .into_iter()
        .map(|d| (route_name_for(&prefix, &d.upstream_id), d))
        .collect();

    match state.audit.models_seen(name, &rows) {
        Ok((added, missing)) => {
            out.added = added;
            out.missing = missing;
        }
        Err(e) => {
            tracing::error!("the catalogue could not be written: {e}");
            out.ok = false;
            out.detail = Some("the catalogue could not be written to the database".to_owned());
        }
    }

    out.ms = started.elapsed().as_millis() as u64;
    out
}

/// An upstream's words, bounded and passed through the scrubber. Same treatment
/// the probe gives an error page, and for the same reason.
fn short_and_clean(state: &AppState, body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let short: String = text.chars().take(300).collect();
    match state.scrubber().substitute(short.as_bytes()) {
        Some(clean) => String::from_utf8_lossy(&clean).into_owned(),
        None => short,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Trust;

    fn provider(base: &str) -> Provider {
        Provider {
            base_url: base.to_owned(),
            key: None,
            auth_header: "x-api-key".to_owned(),
            bearer: false,
            wire: crate::wire::Wire::Anthropic,
            trust: Trust::Low,
            timeout_secs: 120,
            prefix: None,
            models_path: None,
            catalog: true,
        }
    }

    fn entry(provider: &str, route: &str, upstream: &str, enabled: bool) -> ModelEntry {
        ModelEntry {
            provider: provider.to_owned(),
            upstream_id: upstream.to_owned(),
            route: route.to_owned(),
            label: None,
            context_length: None,
            enabled,
            source: Source::Probed,
            present: true,
            first_seen: "2026-08-13T00:00:00Z".to_owned(),
            last_seen: "2026-08-13T00:00:00Z".to_owned(),
            note: None,
        }
    }

    #[test]
    fn both_dialects_publish_the_same_envelope() {
        let anthropic = br#"{"data":[{"type":"model","id":"claude-opus-5","display_name":"Opus 5"}],"has_more":false}"#;
        let got = parse_catalogue(anthropic).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].upstream_id, "claude-opus-5");
        assert_eq!(got[0].label.as_deref(), Some("Opus 5"));

        let openai = br#"{"object":"list","data":[{"id":"deepseek-v4","object":"model","context_length":1000000}]}"#;
        let got = parse_catalogue(openai).unwrap();
        assert_eq!(got[0].context_length, Some(1_000_000));
        assert!(got[0].label.is_none());
    }

    #[test]
    fn an_unreadable_catalogue_is_an_error_and_not_an_empty_list() {
        // The distinction is the whole point: "this token sells nothing" and
        // "douane could not read the answer" must not look the same on screen.
        assert!(parse_catalogue(b"<html>nope</html>").is_err());
        assert!(parse_catalogue(br#"{"error":{"message":"bad key"}}"#).is_err());
        assert_eq!(parse_catalogue(br#"{"data":[]}"#).unwrap().len(), 0);
    }

    #[test]
    fn a_bare_array_of_names_is_read_too() {
        let got = parse_catalogue(br#"["a-model","b-model"]"#).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].upstream_id, "b-model");
    }

    #[test]
    fn a_label_that_only_repeats_the_id_is_dropped() {
        let got = parse_catalogue(br#"{"data":[{"id":"x","display_name":"x"}]}"#).unwrap();
        assert!(got[0].label.is_none());
    }

    #[test]
    fn route_names_are_prefixed_and_safe_to_type() {
        assert_eq!(route_name_for("crof-", "deepseek/v4:pro"), "crof-deepseek-v4-pro");
        assert_eq!(route_name_for("ol-", "gpt-5.6-terra"), "ol-gpt-5.6-terra");
        // A leading or trailing separator would produce a name ending in a dash.
        assert_eq!(route_name_for("cx-", "/weird/"), "cx-weird");
    }

    #[test]
    fn a_disabled_model_is_not_a_route() {
        let mut c = Config {
            server: Default::default(),
            providers: BTreeMap::new(),
            routes: BTreeMap::new(),
            clients: BTreeMap::new(),
            scrub: Default::default(),
            egress: Default::default(),
        };
        c.providers.insert("crof".into(), provider("https://crof.ai/v1"));
        let models = vec![
            entry("crof", "crof-a", "a", false),
            entry("crof", "crof-b", "b", true),
        ];
        let live = c.with_models(&models);
        assert!(!live.routes.contains_key("crof-a"));
        assert_eq!(live.routes["crof-b"].model.as_deref(), Some("b"));
        live.validate().unwrap();
    }

    #[test]
    fn the_file_wins_a_collision_and_the_conflict_is_reported() {
        // An upstream that starts publishing an id matching a hand-written
        // route must never be able to repoint it.
        let mut c = Config {
            server: Default::default(),
            providers: BTreeMap::new(),
            routes: BTreeMap::new(),
            clients: BTreeMap::new(),
            scrub: Default::default(),
            egress: Default::default(),
        };
        c.providers.insert("crof".into(), provider("https://crof.ai/v1"));
        c.providers.insert("local".into(), provider("http://10.0.0.1"));
        c.routes.insert(
            "crof-a".into(),
            Route {
                provider: "local".into(),
                model: Some("house".into()),
                fallback: Vec::new(),
            },
        );
        let models = vec![
            entry("crof", "crof-a", "a", true),
            entry("ghost", "ghost-a", "a", true),
        ];
        let live = c.with_models(&models);
        assert_eq!(live.routes["crof-a"].provider, "local");
        assert!(!live.routes.contains_key("ghost-a"));
        let clashes = c.model_collisions(&models);
        assert_eq!(clashes.len(), 2);
        assert!(clashes["crof-a"].contains("douane.toml"));
        assert!(clashes["ghost-a"].contains("provider `ghost`"));
    }

    #[test]
    fn the_catalogue_url_follows_the_dialect_unless_told_otherwise() {
        let mut p = provider("https://api.example.com/");
        assert_eq!(
            p.catalog_url().as_deref(),
            Some("https://api.example.com/v1/models")
        );
        p.wire = crate::wire::Wire::Openai;
        p.base_url = "https://crof.ai/v1".into();
        assert_eq!(p.catalog_url().as_deref(), Some("https://crof.ai/v1/models"));
        p.models_path = Some("https://xcode.best/api/pricing".into());
        assert_eq!(
            p.catalog_url().as_deref(),
            Some("https://xcode.best/api/pricing")
        );
        p.catalog = false;
        assert!(p.catalog_url().is_none());
    }
}
