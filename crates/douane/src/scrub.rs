//! The outbound scrubber.
//!
//! Everything else in douane stops a provider from *keeping* a credential it
//! was handed. This stops one from being handed over in the first place. The
//! leak it exists for is mundane and likely: an agent reads a `.env`, pastes it
//! into its own context to reason about it, and the whole conversation goes to
//! a reseller on the next turn. No amount of key brokering helps there, because
//! the secret is in the message body, not in the header.
//!
//! Two detectors, deliberately unequal:
//!
//! - **A value from the vault, verbatim.** No false positives are possible, so
//!   this one blocks by default. If the user's own Proxy password is on its
//!   way to a reseller, there is no reading of that which is fine.
//! - **Something merely shaped like a credential.** A vendor prefix is strong
//!   evidence but not proof, and a coding agent legitimately handles fake keys,
//!   fixtures and documentation. So this one warns by default and blocks only
//!   if the user says so.
//!
//! Nothing here ever logs, returns or stores the matched text. A hit names the
//! vault reference or the pattern label, and that is all that leaves.

use std::collections::HashSet;

use douane_vault::{SecretBackend, SecretRef};
use regex::Regex;
use serde::Serialize;

use crate::config::{Action, ScrubConfig, Shapes, Trust};

/// The shortest vault value worth searching for.
///
/// Below this, a "secret" is something like a username or a port and matching
/// it produces noise on every request. The threshold is on the value, never on
/// the key name, because a short API key is still an API key.
const MIN_VALUE_LEN: usize = 12;

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    /// `vault` or `shape`.
    pub kind: &'static str,
    /// `proxy.password`, or `anthropic-api-key`. Never the matched text.
    pub label: String,
    /// Byte offset into the outgoing body, so a human can go and look.
    pub at: usize,
}

pub struct Scrubber {
    /// Values from the vault, longest first so the most specific match wins
    /// when one credential is a prefix of another.
    known: Vec<(SecretRef, String)>,
    shapes: Vec<(&'static str, Regex)>,
    config: ScrubConfig,
}

impl Scrubber {
    pub fn build(vault: &dyn SecretBackend, config: ScrubConfig) -> Self {
        let mut known = Vec::new();
        if config.enabled {
            // The one bulk read of the vault in the whole program. It happens
            // once, at startup, and the values never leave this struct.
            vault.for_each_value(&mut |r, secret| {
                if secret.len() >= MIN_VALUE_LEN {
                    known.push((r.clone(), secret.expose().to_owned()));
                }
            });
            known.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
        }

        Self {
            known,
            shapes: shapes_for(config.shapes),
            config,
        }
    }

    /// How many vault values are being watched for. Safe to log and to show in
    /// the dashboard: it is a count, not a listing.
    pub fn watching(&self) -> usize {
        self.known.len()
    }

    /// Look at a body about to leave for a provider of the given trust level.
    ///
    /// Returns every hit found and the strongest action any of them calls for.
    pub fn inspect(&self, body: &[u8], trust: Trust) -> (Vec<Hit>, Action) {
        if !self.config.enabled {
            return (Vec::new(), Action::Allow);
        }
        let Ok(text) = std::str::from_utf8(body) else {
            // A body that is not text is not a body this broker understands, and
            // the relay will fail on it anyway.
            return (Vec::new(), Action::Allow);
        };

        let vault_action = self.config.vault_hit.at(trust);
        let shape_action = self.config.shape_hit.at(trust);

        let mut hits = Vec::new();
        let mut worst = Action::Allow;

        if vault_action != Action::Allow {
            // Regions already claimed by a longer match, so one credential that
            // contains another is reported once, under the longer name.
            let mut claimed: Vec<(usize, usize)> = Vec::new();
            for (r, value) in &self.known {
                for (at, _) in text.match_indices(value.as_str()) {
                    let span = (at, at + value.len());
                    if claimed.iter().any(|c| span.0 >= c.0 && span.1 <= c.1) {
                        continue;
                    }
                    claimed.push(span);
                    hits.push(Hit {
                        kind: "vault",
                        label: r.to_string(),
                        at,
                    });
                    worst = worst.max(vault_action);
                }
            }
        }

        if shape_action != Action::Allow {
            // One hit per pattern. A leaked file can hold forty keys and forty
            // identical alerts say nothing the first one did not.
            let mut seen = HashSet::new();
            for (label, re) in &self.shapes {
                if let Some(m) = re.find(text)
                    && seen.insert(*label)
                {
                    hits.push(Hit {
                        kind: "shape",
                        label: (*label).to_owned(),
                        at: m.start(),
                    });
                    worst = worst.max(shape_action);
                }
            }
        }

        (hits, worst)
    }
}

/// Vendor prefixes only, by default.
///
/// Every pattern here is anchored on a prefix a vendor actually issues, so a
/// match is strong evidence rather than a guess. The aggressive set adds two
/// that fire on ordinary source code, which is why they are not on by default:
/// an agent reading a test fixture would trip them all day.
fn shapes_for(level: Shapes) -> Vec<(&'static str, Regex)> {
    if level == Shapes::Off {
        return Vec::new();
    }

    let mut out: Vec<(&'static str, &'static str)> = vec![
        ("anthropic-api-key", r"sk-ant-[A-Za-z0-9_\-]{24,}"),
        // The tail of the legacy form is deliberately alphanumeric only. Letting
        // it take dashes makes it swallow every `sk-ant-...` key too, and the
        // crate has no lookahead to exclude one prefix from another.
        (
            "openai-api-key",
            r"sk-(?:proj-[A-Za-z0-9_\-]{32,}|[A-Za-z0-9]{32,})",
        ),
        ("github-token", r"gh[pousr]_[A-Za-z0-9]{36}"),
        ("github-pat", r"github_pat_[A-Za-z0-9_]{60,}"),
        ("gitlab-token", r"glpat-[A-Za-z0-9_\-]{20,}"),
        ("aws-access-key-id", r"AKIA[0-9A-Z]{16}"),
        ("slack-token", r"xox[baprs]-[A-Za-z0-9\-]{10,}"),
        ("google-api-key", r"AIza[0-9A-Za-z_\-]{35}"),
        ("stripe-secret-key", r"sk_live_[A-Za-z0-9]{24,}"),
        ("digitalocean-token", r"dop_v1_[a-f0-9]{64}"),
        ("private-key-pem", r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
    ];

    if level == Shapes::Aggressive {
        out.push((
            "jwt",
            r"eyJ[A-Za-z0-9_\-]{10,}\.eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}",
        ));
        out.push((
            "credential-assignment",
            r#"(?i)(?:api[_\-]?key|secret|password|passwd|token)["' ]*[:=]["' ]*[A-Za-z0-9/+_=\-]{20,}"#,
        ));
    }

    out.into_iter()
        .map(|(label, pattern)| {
            // Every pattern here is a literal in this file. A build that cannot
            // compile one is broken, not misconfigured, so panicking is right.
            (label, Regex::new(pattern).expect("built-in scrub pattern"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ByTrust, ScrubConfig};
    use douane_vault::{Entry, EntryInfo, Secret, VaultError};

    /// A vault stub. The real one needs a passphrase and 64 MiB of Argon2 per
    /// open, which is not what these tests are about.
    struct Fake(Vec<(SecretRef, Secret)>);

    impl SecretBackend for Fake {
        fn get(&self, r: &SecretRef) -> Option<&Secret> {
            self.0.iter().find(|(k, _)| k == r).map(|(_, v)| v)
        }
        fn put(&mut self, _r: &SecretRef, _e: Entry) {}
        fn delete(&mut self, _r: &SecretRef) -> bool {
            false
        }
        fn namespaces(&self) -> Vec<String> {
            Vec::new()
        }
        fn list(&self) -> Vec<EntryInfo> {
            Vec::new()
        }
        fn persist(&self) -> Result<(), VaultError> {
            Ok(())
        }
        fn for_each_value(&self, f: &mut dyn FnMut(&SecretRef, &Secret)) {
            for (r, s) in &self.0 {
                f(r, s);
            }
        }
    }

    fn scrubber() -> Scrubber {
        let vault = Fake(vec![
            (
                SecretRef::new("proxy", "password"),
                Secret::new("Tr0ub4dor-and-3-more"),
            ),
            // Too short to be worth watching: it would match inside ordinary
            // prose and drown the real hits.
            (SecretRef::new("nas", "user"), Secret::new("operator")),
        ]);
        Scrubber::build(&vault, ScrubConfig::default())
    }

    #[test]
    fn short_values_are_not_watched() {
        let s = scrubber();
        assert_eq!(s.watching(), 1);
        let (hits, _) = s.inspect(b"the user is operator", Trust::Low);
        assert!(hits.is_empty());
    }

    #[test]
    fn a_vault_value_heading_for_a_reseller_is_blocked() {
        let s = scrubber();
        let body = br#"{"messages":[{"content":"the password is Tr0ub4dor-and-3-more ok"}]}"#;
        let (hits, action) = s.inspect(body, Trust::Low);
        assert_eq!(action, Action::Block);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].label, "proxy.password");
        assert_eq!(hits[0].kind, "vault");
    }

    #[test]
    fn the_hit_never_carries_the_value() {
        let s = scrubber();
        let (hits, _) = s.inspect(b"Tr0ub4dor-and-3-more", Trust::Low);
        let json = serde_json::to_string(&hits).unwrap();
        assert!(!json.contains("Tr0ub4dor"));
        assert!(json.contains("proxy.password"));
    }

    #[test]
    fn the_local_model_is_left_alone() {
        // Nothing leaves the LAN, so scanning would only cost latency and
        // produce alerts about the user talking to their own hardware.
        let s = scrubber();
        let (hits, action) = s.inspect(b"Tr0ub4dor-and-3-more", Trust::Max);
        assert!(hits.is_empty());
        assert_eq!(action, Action::Allow);
    }

    #[test]
    fn vendor_prefixes_warn_but_do_not_block() {
        let s = scrubber();
        let body = b"here is the key sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA and a token";
        let (hits, action) = s.inspect(body, Trust::Low);
        assert_eq!(action, Action::Warn);
        // Exactly one hit: an Anthropic key also reads as an OpenAI one unless
        // the OpenAI pattern is written to exclude it, and two alerts for one
        // key is how an alert list stops being read.
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].label, "anthropic-api-key");
    }

    #[test]
    fn each_vendor_key_is_reported_under_one_name() {
        let s = scrubber();
        for (key, expected) in [
            (
                "sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                "anthropic-api-key",
            ),
            (
                "sk-proj-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                "openai-api-key",
            ),
            (
                "sk-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                "openai-api-key",
            ),
            ("ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "github-token"),
        ] {
            let (hits, _) = s.inspect(key.as_bytes(), Trust::Low);
            assert_eq!(hits.len(), 1, "{key} matched {hits:?}");
            assert_eq!(hits[0].label, expected);
        }
    }

    #[test]
    fn a_pem_header_is_caught_by_the_default_set() {
        let s = scrubber();
        let (hits, _) = s.inspect(b"-----BEGIN OPENSSH PRIVATE KEY-----\nb3Blbn", Trust::Low);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].label, "private-key-pem");
    }

    #[test]
    fn source_code_does_not_trip_the_default_set() {
        // The exact thing an agent sends all day. The aggressive set is opt-in
        // precisely so this stays quiet.
        let s = scrubber();
        let body = br#"const config = { password: "changeme", apiKey: process.env.API_KEY };"#;
        let (hits, _) = s.inspect(body, Trust::Low);
        assert!(hits.is_empty());
    }

    #[test]
    fn the_aggressive_set_does_trip_on_it() {
        let vault = Fake(Vec::new());
        let config = ScrubConfig {
            shapes: Shapes::Aggressive,
            shape_hit: ByTrust {
                low: Action::Block,
                mid: Action::Allow,
                max: Action::Allow,
            },
            ..ScrubConfig::default()
        };
        let s = Scrubber::build(&vault, config);
        let body = br#"api_key = "AKIAIOSFODNN7EXAMPLEabcdefgh""#;
        let (hits, action) = s.inspect(body, Trust::Low);
        assert_eq!(action, Action::Block);
        assert!(hits.iter().any(|h| h.label == "credential-assignment"));
    }

    #[test]
    fn one_alert_per_pattern_however_many_keys_leak() {
        let s = scrubber();
        let body = b"AKIAAAAAAAAAAAAAAAAA AKIABBBBBBBBBBBBBBBB AKIACCCCCCCCCCCCCCCC";
        let (hits, _) = s.inspect(body, Trust::Low);
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn disabling_it_really_disables_it() {
        let vault = Fake(vec![(
            SecretRef::new("a", "b"),
            Secret::new("Tr0ub4dor-and-3-more"),
        )]);
        let config = ScrubConfig {
            enabled: false,
            ..ScrubConfig::default()
        };
        let s = Scrubber::build(&vault, config);
        assert_eq!(s.watching(), 0);
        let (hits, action) = s.inspect(b"Tr0ub4dor-and-3-more", Trust::Low);
        assert!(hits.is_empty());
        assert_eq!(action, Action::Allow);
    }
}
