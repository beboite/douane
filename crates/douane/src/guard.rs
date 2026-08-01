//! Where a provider is allowed to actually be.
//!
//! A provider is named by hostname, and a hostname is a promise the DNS makes
//! and can stop keeping. On this network it stops keeping it in the worst
//! possible way: the resolver carries a search domain, the local DNS answers a
//! wildcard under it, and so **a name that fails to resolve comes back as a LAN
//! address** rather than as an error. `nimportequoi.github.com` answers
//! `10.0.0.20`, which is a reverse proxy.
//!
//! Put a reseller's domain into that and the consequence is not a broken
//! request, it is a credential leak: douane would open a connection to the
//! local proxy and put the real API key in the header, where it lands in an
//! access log. The request would even look like it worked.
//!
//! So an upstream that is supposed to be on the internet has to prove it is.
//! Checked at startup and by `--check`, which is the moment a lapsed domain
//! will actually be noticed, and fatal only when a credential is at stake: a
//! provider with no key configured has nothing to leak and only gets a warning.

use std::net::IpAddr;

use crate::config::{Config, Trust};

/// An address that cannot be a public API endpoint.
///
/// Loopback, the three private ranges, link-local and carrier NAT for v4;
/// loopback, unique-local and link-local for v6. Deliberately broad: the
/// question is not "is this routable" but "could a credential sent here be read
/// by something on this network".
pub fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                // 100.64.0.0/10, the tailnet lives here.
                || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // fc00::/7, unique local. `is_unique_local` is still unstable.
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // fe80::/10, link local.
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || v6.to_ipv4_mapped().is_some_and(|m| is_local(IpAddr::V4(m)))
        }
    }
}

/// What went wrong with one provider.
pub struct Problem {
    pub provider: String,
    pub detail: String,
    /// True when a credential would have been handed over. Fatal.
    pub keyed: bool,
}

/// Resolve every provider that is meant to be off-network and confirm it is.
///
/// A provider at `trust = "max"` is skipped: it is *supposed* to be on the LAN,
/// that is the whole meaning of the level.
pub async fn check_providers(config: &Config) -> Vec<Problem> {
    let mut problems = Vec::new();

    for (name, provider) in &config.providers {
        if provider.trust == Trust::Max {
            continue;
        }
        let Ok(url) = reqwest::Url::parse(&provider.base_url) else {
            problems.push(Problem {
                provider: name.clone(),
                detail: format!("`{}` is not a URL", provider.base_url),
                keyed: provider.key.is_some(),
            });
            continue;
        };
        let Some(host) = url.host_str() else {
            problems.push(Problem {
                provider: name.clone(),
                detail: format!("`{}` names no host", provider.base_url),
                keyed: provider.key.is_some(),
            });
            continue;
        };

        // An address literal needs no resolution and is checked as written.
        if let Ok(ip) = host.parse::<IpAddr>() {
            if is_local(ip) {
                problems.push(Problem {
                    provider: name.clone(),
                    detail: format!(
                        "{host} is a local address, but this provider is not trusted `max`"
                    ),
                    keyed: provider.key.is_some(),
                });
            }
            continue;
        }

        let port = url.port_or_known_default().unwrap_or(443);
        match tokio::net::lookup_host((host, port)).await {
            Ok(addrs) => {
                let local: Vec<String> = addrs
                    .map(|a| a.ip())
                    .filter(|ip| is_local(*ip))
                    .map(|ip| ip.to_string())
                    .collect();
                if !local.is_empty() {
                    problems.push(Problem {
                        provider: name.clone(),
                        detail: format!(
                            "{host} resolves to {}, which is on this network. \
                             Either the domain has lapsed and the resolver's search \
                             domain answered for it, or someone is answering in its place",
                            local.join(", ")
                        ),
                        keyed: provider.key.is_some(),
                    });
                }
            }
            Err(e) => {
                // Not fatal even with a key: DNS being down at boot is ordinary
                // and refusing to start would turn a blip into an outage. The
                // dangerous case is the one that answers, not the one that does
                // not.
                problems.push(Problem {
                    provider: name.clone(),
                    detail: format!("{host} does not resolve right now ({e})"),
                    keyed: false,
                });
            }
        }
    }

    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn the_house_is_recognised_as_the_house() {
        for s in [
            "127.0.0.1",
            "10.0.0.10",
            "172.16.4.4",
            "10.0.0.20",
            "169.254.1.1",
            "100.64.0.11",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.20",
        ] {
            assert!(is_local(s.parse().unwrap()), "{s} should read as local");
        }
    }

    #[test]
    fn the_internet_is_not() {
        for s in [
            "1.1.1.1",
            "8.8.8.8",
            "104.21.89.237",
            "172.15.0.1",
            "172.32.0.1",
            "99.64.0.1",
            "2606:4700:3031::ac43:bfec",
            "2a06:98c1:3121::2",
        ] {
            assert!(!is_local(s.parse().unwrap()), "{s} should read as public");
        }
    }

    #[test]
    fn the_carrier_range_boundaries_are_right() {
        // 100.64.0.0/10 is 100.64 through 100.127, and 100.128 is public.
        assert!(is_local(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 0))));
        assert!(is_local(IpAddr::V4(Ipv4Addr::new(100, 127, 255, 255))));
        assert!(!is_local(IpAddr::V4(Ipv4Addr::new(100, 63, 255, 255))));
        assert!(!is_local(IpAddr::V4(Ipv4Addr::new(100, 128, 0, 0))));
    }

    #[test]
    fn unique_local_v6_boundaries_are_right() {
        assert!(is_local(IpAddr::V6("fc00::1".parse::<Ipv6Addr>().unwrap())));
        assert!(is_local(IpAddr::V6("fdff::1".parse::<Ipv6Addr>().unwrap())));
        assert!(!is_local(IpAddr::V6(
            "fe00::1".parse::<Ipv6Addr>().unwrap()
        )));
    }

    fn config_with(base_url: &str, trust: Trust, keyed: bool) -> Config {
        let mut c: Config = toml::from_str(
            r#"
[providers.p]
base_url = "https://example.invalid"
trust = "low"
[routes.r]
provider = "p"
[clients.c]
key = { namespace = "douane", key = "vk" }
"#,
        )
        .unwrap();
        let p = c.providers.get_mut("p").unwrap();
        p.base_url = base_url.to_owned();
        p.trust = trust;
        p.key = keyed.then(|| douane_vault::SecretRef::new("p", "api_key"));
        c
    }

    #[tokio::test]
    async fn a_literal_lan_address_on_a_low_trust_provider_is_a_problem() {
        let c = config_with("http://10.0.0.20", Trust::Low, true);
        let problems = check_providers(&c).await;
        assert_eq!(problems.len(), 1);
        assert!(problems[0].keyed, "a key was configured, so this is fatal");
        assert!(problems[0].detail.contains("10.0.0.20"));
    }

    #[tokio::test]
    async fn the_same_address_is_fine_for_the_house_model() {
        // `max` means "on hardware the user owns". Flagging it would be
        // flagging the one provider that is meant to be there.
        let c = config_with("http://10.0.0.20", Trust::Max, true);
        assert!(check_providers(&c).await.is_empty());
    }

    #[tokio::test]
    async fn a_provider_with_no_key_has_nothing_to_leak() {
        let c = config_with("http://127.0.0.1:8080", Trust::Low, false);
        let problems = check_providers(&c).await;
        assert_eq!(problems.len(), 1);
        assert!(
            !problems[0].keyed,
            "no credential, so a warning and not a stop"
        );
    }

    #[tokio::test]
    async fn a_name_that_does_not_resolve_is_reported_but_never_fatal() {
        // DNS down at boot is ordinary. Refusing to start would turn a blip
        // into an outage, and a name that does not answer cannot receive a key.
        let c = config_with("https://nothing-here.invalid", Trust::Low, true);
        let problems = check_providers(&c).await;
        assert_eq!(problems.len(), 1);
        assert!(!problems[0].keyed);
        assert!(problems[0].detail.contains("does not resolve"));
    }
}
