//! The certificate authority the egress broker signs with.
//!
//! Today douane only sees traffic that was pointed at it. An agent that calls
//! `api.anthropic.com` directly, or posts a `.env` to some unrelated API, walks
//! straight past the scrubber. Closing that means terminating TLS on the way
//! out, which means holding an authority whose certificates the agent's machine
//! is willing to believe.
//!
//! That authority is the most dangerous thing in this repository. Anything that
//! trusts it hands douane every TLS session it opens, so:
//!
//! - **The private key lives in the vault**, never on disk beside the
//!   certificate. It is a credential like any other and the same rules apply.
//! - **The certificate does not.** It is public by construction, has to be
//!   copied into trust stores, and putting it in the vault would only teach the
//!   scrubber to block the one file that is meant to be handed around.
//! - **Leaves are short lived.** A minted certificate outliving the process that
//!   made it is a stolen credential waiting to happen, and nothing needs one to
//!   survive a restart.
//! - **Nothing here installs anything.** Adding a CA to a machine's trust store
//!   is the owner's decision to take deliberately, not a side effect of running
//!   a broker.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use time::{Duration, OffsetDateTime};

/// How long a minted host certificate is good for.
///
/// Short on purpose. These are handed to whatever opened the connection, and a
/// year-long certificate for `api.anthropic.com` signed by a CA the machine
/// trusts is exactly the artefact an attacker would want to walk off with.
const LEAF_DAYS: i64 = 2;

/// How long the authority itself is good for.
///
/// Long enough that rotating it is a scheduled job rather than a weekly
/// interruption, short enough that an abandoned instance stops working instead
/// of quietly staying trusted forever.
const CA_DAYS: i64 = 365;

/// A generated authority, at the one moment both halves are in hand.
///
/// Returned by value so the caller decides where each half goes: the key into
/// the vault, the certificate onto disk. Deliberately not a struct that can sit
/// around holding both.
pub struct NewAuthority {
    /// PEM certificate. Public. This is what gets copied into a trust store.
    pub cert_pem: String,
    /// PEM private key. Belongs in the vault and nowhere else.
    pub key_pem: String,
}

/// Mints host certificates for the egress broker.
pub struct Authority {
    issuer: Issuer<'static, KeyPair>,
    /// PEM of the authority's own certificate, kept so a caller can show it
    /// without reading the file back.
    cert_pem: String,
    /// One certificate per host name, minted on first sight.
    ///
    /// A `Mutex` and not a lock-free map: minting is rare, and holding the lock
    /// across a signature is cheaper than the machinery to avoid it.
    cache: Mutex<HashMap<String, Minted>>,
}

/// A host certificate and the key that goes with it, both PEM.
#[derive(Clone)]
pub struct Minted {
    pub cert_pem: String,
    pub key_pem: String,
}

impl Authority {
    /// Make a fresh authority. Called once, by hand, through `douanectl`.
    pub fn generate(name: &str) -> Result<NewAuthority> {
        let key = KeyPair::generate().context("generating the authority key")?;

        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        // Signing certificates and signing revocation lists, and nothing else.
        // A CA that can also do key agreement is a CA that can be used as a
        // server certificate by anything that does not check.
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        params.distinguished_name = {
            let mut dn = DistinguishedName::new();
            dn.push(DnType::CommonName, name);
            dn.push(DnType::OrganizationName, "douane");
            dn
        };
        let now = OffsetDateTime::now_utc();
        params.not_before = now - Duration::hours(1);
        params.not_after = now + Duration::days(CA_DAYS);

        let cert = params.self_signed(&key).context("self-signing it")?;
        Ok(NewAuthority {
            cert_pem: cert.pem(),
            key_pem: key.serialize_pem(),
        })
    }

    /// Rebuild an authority from the certificate on disk and the key from the
    /// vault.
    pub fn load(cert_pem: &str, key_pem: &str) -> Result<Self> {
        // rcgen's own loader documents that it assumes what it is given is a CA
        // and checks nothing. Pointing this at a host certificate would
        // otherwise succeed here and fail much later as a handshake error with
        // no obvious cause, so the check happens where the mistake is made.
        if !is_a_certificate_authority(cert_pem)? {
            bail!("that certificate is not a certificate authority");
        }
        let key = KeyPair::from_pem(key_pem).context("reading the authority key")?;
        Ok(Self {
            issuer: Issuer::from_ca_cert_pem(cert_pem, key)
                .context("reading the authority certificate")?,
            cert_pem: cert_pem.to_owned(),
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// The authority's own certificate, PEM. Public, and meant to be copied.
    pub fn cert_pem(&self) -> &str {
        &self.cert_pem
    }

    /// How many host certificates have been minted since startup. The only
    /// thing the cache exposes: the names themselves are what an agent is
    /// talking to, and a getter for those would be a listing of its traffic.
    pub fn minted(&self) -> usize {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// A certificate for `host`, minted on first sight and reused after.
    ///
    /// Rejects anything that is not a plain DNS name. `host` arrives from a
    /// `CONNECT` line written by whatever is being proxied, so it is untrusted
    /// input, and a wildcard or an empty label in a subject alternative name is
    /// how one certificate ends up covering more than it was asked for.
    pub fn cert_for(&self, host: &str) -> Result<Minted> {
        let host = host.to_ascii_lowercase();
        if !is_plain_dns_name(&host) {
            bail!("`{host}` is not a host name a certificate can be minted for");
        }

        if let Some(hit) = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&host)
        {
            return Ok(hit.clone());
        }

        let key = KeyPair::generate().context("generating a host key")?;
        let mut params =
            CertificateParams::new(vec![host.clone()]).context("building the host certificate")?;
        params.distinguished_name = {
            let mut dn = DistinguishedName::new();
            dn.push(DnType::CommonName, host.clone());
            dn
        };
        let now = OffsetDateTime::now_utc();
        // Backdated an hour so a client whose clock runs slow still accepts it.
        params.not_before = now - Duration::hours(1);
        params.not_after = now + Duration::days(LEAF_DAYS);

        let cert = params
            .signed_by(&key, &self.issuer)
            .context("signing the host certificate")?;
        let minted = Minted {
            cert_pem: cert.pem(),
            key_pem: key.serialize_pem(),
        };

        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(host, minted.clone());
        Ok(minted)
    }
}

/// Does this PEM certificate carry `basicConstraints: CA:TRUE`?
fn is_a_certificate_authority(cert_pem: &str) -> Result<bool> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes())
        .context("that is not a PEM certificate")?;
    let (_, cert) = x509_parser::parse_x509_certificate(&pem.contents)
        .context("that is not an X.509 certificate")?;
    Ok(cert
        .basic_constraints()
        .ok()
        .flatten()
        .is_some_and(|ext| ext.value.ca))
}

/// Is this a DNS name and only a DNS name?
///
/// No wildcards, no IP addresses, no empty labels, nothing outside
/// letters-digits-hyphen. The rule is deliberately stricter than what the DNS
/// allows: this decides what a certificate is minted for, and the cost of
/// refusing an unusual but legitimate name is a failed connection, while the
/// cost of accepting `*` is one certificate that answers for everything.
fn is_plain_dns_name(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 || host.parse::<std::net::IpAddr>().is_ok() {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn an_authority() -> Authority {
        let new = Authority::generate("douane egress (test)").unwrap();
        Authority::load(&new.cert_pem, &new.key_pem).unwrap()
    }

    #[test]
    fn a_generated_authority_can_be_loaded_back() {
        let new = Authority::generate("douane egress (test)").unwrap();
        assert!(new.cert_pem.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(new.key_pem.contains("PRIVATE KEY"));
        let ca = Authority::load(&new.cert_pem, &new.key_pem).unwrap();
        assert_eq!(ca.cert_pem(), new.cert_pem);
    }

    #[test]
    fn a_leaf_is_not_accepted_as_an_authority() {
        // The failure this catches: pointing the loader at the host certificate
        // instead of the CA. Everything downstream would then sign with a
        // certificate no client will chain, and the error would surface as a
        // TLS handshake failure far from its cause.
        let ca = an_authority();
        let leaf = ca.cert_for("api.anthropic.com").unwrap();
        let err = Authority::load(&leaf.cert_pem, &leaf.key_pem)
            .err()
            .expect("a host certificate must not load as an authority");
        assert!(
            err.to_string().contains("not a certificate authority"),
            "{err}"
        );
    }

    #[test]
    fn one_certificate_per_host_and_the_same_one_twice() {
        let ca = an_authority();
        let a = ca.cert_for("api.anthropic.com").unwrap();
        let b = ca.cert_for("api.anthropic.com").unwrap();
        assert_eq!(a.cert_pem, b.cert_pem, "the second call is a cache hit");
        assert_eq!(ca.minted(), 1);

        let other = ca.cert_for("xcode.best").unwrap();
        assert_ne!(a.cert_pem, other.cert_pem);
        assert_eq!(ca.minted(), 2);
    }

    #[test]
    fn a_host_name_is_matched_regardless_of_case() {
        // `CONNECT` lines are written by clients, and clients disagree about
        // case. Two cache entries for one host is a leak of certificates, not
        // just waste.
        let ca = an_authority();
        let lower = ca.cert_for("api.anthropic.com").unwrap();
        let upper = ca.cert_for("API.Anthropic.COM").unwrap();
        assert_eq!(lower.cert_pem, upper.cert_pem);
        assert_eq!(ca.minted(), 1);
    }

    #[test]
    fn nothing_but_a_plain_dns_name_gets_a_certificate() {
        let ca = an_authority();
        for bad in [
            "",
            "*",
            "*.anthropic.com",
            "api..anthropic.com",
            ".anthropic.com",
            "anthropic.com.",
            "-api.anthropic.com",
            "api-.anthropic.com",
            "api.anthropic.com/../evil",
            "api anthropic com",
            "10.0.0.20",
            "::1",
        ] {
            assert!(
                ca.cert_for(bad).is_err(),
                "`{bad}` should not get a certificate"
            );
        }
        assert_eq!(ca.minted(), 0, "and none of them landed in the cache");
    }

    /// Parse a PEM certificate the way a client would, rather than trusting the
    /// parameters we asked rcgen for. The question is what came out, not what
    /// went in.
    fn parsed(pem: &str) -> x509_parser::certificate::X509Certificate<'static> {
        let (_, doc) = x509_parser::pem::parse_x509_pem(pem.as_bytes()).unwrap();
        let der: &'static [u8] = Box::leak(doc.contents.into_boxed_slice());
        x509_parser::parse_x509_certificate(der).unwrap().1
    }

    #[test]
    fn the_authority_cannot_mint_another_authority() {
        // A path length of zero means it signs leaves and nothing else. Without
        // it, a stolen key does not merely impersonate hosts, it delegates the
        // power to impersonate hosts.
        let new = Authority::generate("douane egress (test)").unwrap();
        let bc = parsed(&new.cert_pem)
            .basic_constraints()
            .unwrap()
            .unwrap()
            .value
            .clone();
        assert!(bc.ca);
        assert_eq!(bc.path_len_constraint, Some(0));
    }

    #[test]
    fn a_minted_certificate_expires_in_days_not_years() {
        let ca = an_authority();
        let minted = ca.cert_for("api.anthropic.com").unwrap();
        let cert = parsed(&minted.cert_pem);
        let life = cert.validity().not_after.timestamp() - cert.validity().not_before.timestamp();
        assert!(
            life <= (LEAF_DAYS + 1) * 86_400,
            "a leaf good for {life}s is a stolen credential waiting to happen"
        );
        // And it is not a CA, whatever else it is.
        assert!(
            cert.basic_constraints()
                .ok()
                .flatten()
                .is_none_or(|bc| !bc.value.ca)
        );
    }

    #[test]
    fn the_minted_name_is_the_one_that_was_asked_for() {
        let ca = an_authority();
        let minted = ca.cert_for("api.anthropic.com").unwrap();
        let san = parsed(&minted.cert_pem)
            .subject_alternative_name()
            .unwrap()
            .unwrap()
            .value
            .general_names
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>();
        assert_eq!(san, ["DNSName(api.anthropic.com)"]);
    }

    #[test]
    fn something_that_is_not_a_certificate_is_refused_rather_than_trusted() {
        for junk in ["", "hello", "-----BEGIN CERTIFICATE-----\nnope\n"] {
            assert!(
                Authority::load(junk, "also not a key").is_err(),
                "`{junk}` should not load as an authority"
            );
        }
    }
}
