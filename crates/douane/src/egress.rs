//! What the egress broker is allowed to do with a connection.
//!
//! The relay in `proxy.rs` only ever sees traffic that was deliberately pointed
//! at it. An agent that calls `api.anthropic.com` itself, or posts a `.env` to
//! some unrelated API, is invisible to it. The egress broker is the other half:
//! the machine's outbound proxy, so that everything leaving is at least looked
//! at.
//!
//! This module is the decision and none of the plumbing. Given a host and a
//! port, it answers with one of three things, and it does so without touching
//! the network so the rule can be tested for what it actually says.
//!
//! The default is deliberately **tunnel, not intercept**. Reading a connection
//! requires presenting a forged certificate, and doing that to everything by
//! default would mean douane sits inside the user's bank session because nobody
//! remembered to write an exception. Interception is opt-in per host, refusal is
//! opt-in per host, and everything else is passed through blind.

use serde::{Deserialize, Serialize};

use crate::config::Trust;

/// What to do with one outbound connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Terminate TLS, look at what is going out, forward it on. Only for hosts
    /// named in the config.
    Intercept(Trust),
    /// Copy bytes both ways without reading them. What douane does with
    /// anything it was not told about.
    Tunnel,
    /// Refuse the connection. For destinations an agent has no business
    /// reaching at all.
    Refuse,
}

/// One rule, matching a host name and everything under it.
///
/// `anthropic.com` covers `api.anthropic.com`. It does not cover
/// `notanthropic.com`, which is the mistake a plain `ends_with` makes and the
/// reason matching is done label by label.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Rule {
    /// The host, or the parent of a family of hosts.
    pub host: String,
    /// What happens to a connection heading there.
    pub action: RuleAction,
    /// How much this destination is trusted with what it is sent. Only
    /// meaningful for `intercept`, where it decides what the scrubber blocks.
    #[serde(default = "default_rule_trust")]
    pub trust: Trust,
    /// Why this rule exists. Read by whoever is wondering whether to delete it.
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleAction {
    Intercept,
    Tunnel,
    Refuse,
}

fn default_rule_trust() -> Trust {
    // A destination named in a rule but not given a trust level is assumed to
    // be the least trustworthy kind. Guessing high here would mean quietly
    // relaxing the scrubber for whoever forgot the field.
    Trust::Low
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EgressConfig {
    /// Where the proxy listens. Absent means the egress broker does not run at
    /// all, which is the default: it needs a certificate authority installed on
    /// the client to be useful, and that is not something to switch on by
    /// accident.
    #[serde(default)]
    pub listen: Option<String>,
    /// Where the authority's certificate is, on disk. Its key comes from the
    /// vault under `douane.ca_key`.
    #[serde(default)]
    pub ca_cert: Option<String>,
    /// Rules, most specific first at match time regardless of the order written
    /// here.
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// Ports that may be tunnelled without being understood. A `CONNECT` to
    /// anything else is refused: a proxy that will open a raw pipe to any port
    /// is a way out of the network, not a way of watching it.
    #[serde(default = "default_ports")]
    pub tunnel_ports: Vec<u16>,
}

fn default_ports() -> Vec<u16> {
    vec![443]
}

impl Default for EgressConfig {
    fn default() -> Self {
        Self {
            listen: None,
            ca_cert: None,
            rules: Vec::new(),
            tunnel_ports: default_ports(),
        }
    }
}

impl EgressConfig {
    /// What to do with a connection to `host:port`.
    ///
    /// The most specific matching rule wins, so `api.anthropic.com` beats
    /// `anthropic.com` whichever order they were written in. Without that, the
    /// meaning of the file would depend on line order, and a rule added at the
    /// bottom would silently do nothing.
    pub fn verdict(&self, host: &str, port: u16) -> Verdict {
        let host = host.trim_end_matches('.').to_ascii_lowercase();

        let best = self
            .rules
            .iter()
            .filter(|r| covers(&r.host.to_ascii_lowercase(), &host))
            .max_by_key(|r| r.host.len());

        match best {
            Some(r) if r.action == RuleAction::Refuse => Verdict::Refuse,
            Some(r) if r.action == RuleAction::Intercept => {
                // Interception is TLS termination, and there is nothing to
                // terminate on a port that is not carrying TLS. Reading it as
                // "tunnel" would be the wrong kind of quiet, so it is refused.
                if port == 443 {
                    Verdict::Intercept(r.trust)
                } else {
                    Verdict::Refuse
                }
            }
            // Named as a tunnel, or not named at all. Either way the port has
            // to be one this proxy is willing to open blindly.
            _ if self.tunnel_ports.contains(&port) => Verdict::Tunnel,
            _ => Verdict::Refuse,
        }
    }

    /// Every problem with the rules as written, so a bad config is rejected at
    /// startup rather than discovered one connection at a time.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();

        if self.listen.is_some() && self.ca_cert.is_none() {
            problems.push(
                "egress: `listen` is set but `ca_cert` is not, so nothing can be intercepted"
                    .to_owned(),
            );
        }
        if self.listen.is_none() && !self.rules.is_empty() {
            problems.push(
                "egress: rules are configured but `listen` is not, so none of them apply"
                    .to_owned(),
            );
        }
        for r in &self.rules {
            if !is_matchable_host(&r.host) {
                problems.push(format!("egress: `{}` is not a host name", r.host));
            }
        }
        // Two rules for one host means one of them does nothing, and which one
        // depends on an ordering nobody should have to reason about.
        let mut seen: Vec<String> = self.rules.iter().map(|r| r.host.to_lowercase()).collect();
        seen.sort();
        for pair in seen.windows(2) {
            if pair[0] == pair[1] {
                problems.push(format!("egress: `{}` has two rules", pair[0]));
            }
        }
        if self.tunnel_ports.contains(&0) {
            problems.push("egress: port 0 is not a port".to_owned());
        }
        problems
    }
}

/// Does `pattern` cover `host`, as a name or as a parent of one?
///
/// Label by label, never as a string suffix. `ends_with("anthropic.com")` also
/// matches `evilanthropic.com`, which is exactly how a rule meant to trust one
/// company ends up trusting whoever registered a lookalike.
fn covers(pattern: &str, host: &str) -> bool {
    if pattern == host {
        return true;
    }
    host.len() > pattern.len()
        && host.ends_with(pattern)
        && host.as_bytes()[host.len() - pattern.len() - 1] == b'.'
}

/// Loose enough for a rule to be written for a parent domain, strict enough
/// that nothing with a wildcard, a slash or a space gets in.
fn is_matchable_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(host: &str, action: RuleAction) -> Rule {
        Rule {
            host: host.to_owned(),
            action,
            trust: Trust::Low,
            note: None,
        }
    }

    fn config(rules: Vec<Rule>) -> EgressConfig {
        EgressConfig {
            listen: Some("127.0.0.1:8888".to_owned()),
            ca_cert: Some("douane-ca.crt".to_owned()),
            rules,
            tunnel_ports: default_ports(),
        }
    }

    #[test]
    fn an_unknown_destination_is_tunnelled_and_not_read() {
        // The default matters more than any rule. Intercepting by default would
        // put douane inside the user's bank session the first time someone
        // forgot to write an exception.
        let c = config(vec![]);
        assert_eq!(c.verdict("bank.example", 443), Verdict::Tunnel);
        assert_eq!(c.verdict("api.anthropic.com", 443), Verdict::Tunnel);
    }

    #[test]
    fn a_rule_covers_the_hosts_underneath_it() {
        let c = config(vec![rule("anthropic.com", RuleAction::Intercept)]);
        assert_eq!(
            c.verdict("anthropic.com", 443),
            Verdict::Intercept(Trust::Low)
        );
        assert_eq!(
            c.verdict("api.anthropic.com", 443),
            Verdict::Intercept(Trust::Low)
        );
        assert_eq!(
            c.verdict("a.b.anthropic.com", 443),
            Verdict::Intercept(Trust::Low)
        );
    }

    #[test]
    fn a_lookalike_domain_is_not_covered() {
        // The bug a suffix match makes: `notanthropic.com` ends with
        // `anthropic.com`, so a rule meant for one company would cover whoever
        // registered the lookalike.
        let c = config(vec![rule("anthropic.com", RuleAction::Intercept)]);
        assert_eq!(c.verdict("notanthropic.com", 443), Verdict::Tunnel);
        assert_eq!(c.verdict("evil-anthropic.com", 443), Verdict::Tunnel);
        assert_eq!(c.verdict("anthropic.com.evil.test", 443), Verdict::Tunnel);
    }

    #[test]
    fn the_most_specific_rule_wins_whatever_the_order() {
        // Written parent first, then child.
        let c = config(vec![
            rule("anthropic.com", RuleAction::Refuse),
            rule("api.anthropic.com", RuleAction::Intercept),
        ]);
        assert_eq!(
            c.verdict("api.anthropic.com", 443),
            Verdict::Intercept(Trust::Low)
        );
        assert_eq!(c.verdict("other.anthropic.com", 443), Verdict::Refuse);

        // And the other way round, for the same answer.
        let c = config(vec![
            rule("api.anthropic.com", RuleAction::Intercept),
            rule("anthropic.com", RuleAction::Refuse),
        ]);
        assert_eq!(
            c.verdict("api.anthropic.com", 443),
            Verdict::Intercept(Trust::Low)
        );
        assert_eq!(c.verdict("other.anthropic.com", 443), Verdict::Refuse);
    }

    #[test]
    fn a_host_is_matched_regardless_of_case_or_a_trailing_dot() {
        // Both come off the wire from clients that disagree about them, and a
        // rule that silently stops applying is worse than one that never did.
        let c = config(vec![rule("anthropic.com", RuleAction::Refuse)]);
        assert_eq!(c.verdict("API.Anthropic.COM", 443), Verdict::Refuse);
        assert_eq!(c.verdict("api.anthropic.com.", 443), Verdict::Refuse);
    }

    #[test]
    fn a_port_nobody_asked_for_is_refused_rather_than_opened() {
        // A proxy that will open a raw pipe to any port on request is a way out
        // of the network, not a way of watching it.
        let c = config(vec![]);
        assert_eq!(c.verdict("anywhere.example", 22), Verdict::Refuse);
        assert_eq!(c.verdict("anywhere.example", 25), Verdict::Refuse);
        assert_eq!(c.verdict("anywhere.example", 8787), Verdict::Refuse);
    }

    #[test]
    fn interception_on_a_port_that_is_not_tls_is_refused_not_tunnelled() {
        // There is nothing to terminate, and quietly tunnelling a host that was
        // explicitly marked for inspection is the wrong kind of quiet.
        let c = config(vec![rule("anthropic.com", RuleAction::Intercept)]);
        assert_eq!(c.verdict("api.anthropic.com", 80), Verdict::Refuse);
    }

    #[test]
    fn a_trust_level_travels_with_the_rule() {
        // It is what the scrubber will use to decide whether a vault value may
        // leave for this destination.
        let mut r = rule("localmodel.lan", RuleAction::Intercept);
        r.trust = Trust::Max;
        let c = config(vec![r, rule("xcode.best", RuleAction::Intercept)]);
        assert_eq!(
            c.verdict("localmodel.lan", 443),
            Verdict::Intercept(Trust::Max)
        );
        assert_eq!(c.verdict("xcode.best", 443), Verdict::Intercept(Trust::Low));
    }

    #[test]
    fn a_rule_with_no_trust_written_down_is_the_least_trusted_kind() {
        let r: Rule = toml::from_str(
            r#"
            host = "somewhere.example"
            action = "intercept"
            "#,
        )
        .unwrap();
        assert_eq!(
            r.trust,
            Trust::Low,
            "guessing high would relax the scrubber"
        );
    }

    #[test]
    fn a_config_that_cannot_do_what_it_says_is_reported() {
        let mut c = config(vec![]);
        c.ca_cert = None;
        assert!(c.validate().iter().any(|p| p.contains("ca_cert")));

        let mut c = config(vec![rule("anthropic.com", RuleAction::Intercept)]);
        c.listen = None;
        assert!(
            c.validate()
                .iter()
                .any(|p| p.contains("none of them apply"))
        );

        let c = config(vec![
            rule("anthropic.com", RuleAction::Intercept),
            rule("ANTHROPIC.com", RuleAction::Refuse),
        ]);
        assert!(c.validate().iter().any(|p| p.contains("two rules")));

        let c = config(vec![rule("*.anthropic.com", RuleAction::Intercept)]);
        assert!(c.validate().iter().any(|p| p.contains("not a host name")));
    }

    #[test]
    fn the_default_config_runs_nothing() {
        // Egress needs a CA installed on the client to be useful. Anything that
        // needs that is not something to switch on by default.
        let c = EgressConfig::default();
        assert!(c.listen.is_none());
        assert!(c.validate().is_empty());
    }
}
