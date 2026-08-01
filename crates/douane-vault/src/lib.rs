//! The douane vault.
//!
//! One encrypted file holds every credential the broker injects on the way out.
//! Nothing that runs an agent ever reads this file: the broker opens it at
//! startup, keeps the plaintext in memory only, and substitutes values into
//! upstream requests after the agent's payload has already left its process.
//!
//! [`SecretBackend`] is the seam. V1 ships [`FileBackend`] and nothing else;
//! a Vaultwarden-backed implementation slots in behind the same trait without
//! the broker knowing which one it holds.

mod file;
mod secret;

pub use file::{FileBackend, KdfParams, MasterKey};
pub use secret::Secret;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Where a secret lives: `proxy` / `password`, `example-reseller` / `api_key`.
///
/// The namespace is the service, the key is the field. This mirrors the layout
/// the brain's `.secrets/credentials.json` already uses, so the one-off import
/// is a rename and not a reshaping.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SecretRef {
    pub namespace: String,
    pub key: String,
}

impl SecretRef {
    pub fn new(namespace: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            key: key.into(),
        }
    }

    /// Parse the `namespace.key` form used in config files and on the CLI.
    ///
    /// The namespace may not contain a dot, so the split is on the first one
    /// and a key like `oauth.refresh_token` stays intact.
    pub fn parse(s: &str) -> Result<Self, VaultError> {
        let (namespace, key) = s
            .split_once('.')
            .ok_or_else(|| VaultError::BadRef(s.to_owned()))?;
        if namespace.is_empty() || key.is_empty() {
            return Err(VaultError::BadRef(s.to_owned()));
        }
        Ok(Self::new(namespace, key))
    }

    /// The placeholder an agent may safely hold in place of the value.
    ///
    /// `proxy` / `password` becomes `__DOUANE_PROXY_PASSWORD__`. The shape
    /// is deliberately loud: it survives being pasted into a config file, and
    /// it is trivially greppable when auditing what an agent actually sent.
    pub fn placeholder(&self) -> String {
        format!(
            "__DOUANE_{}_{}__",
            sanitize(&self.namespace),
            sanitize(&self.key)
        )
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

impl std::fmt::Display for SecretRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.namespace, self.key)
    }
}

/// One stored credential, plus the metadata the dashboard needs to show it
/// without ever showing the value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub value: Secret,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Entry {
    pub fn new(value: Secret) -> Self {
        Self {
            value,
            updated_at: OffsetDateTime::now_utc(),
            note: None,
        }
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }
}

/// What a caller may know about a secret without holding it.
///
/// Every listing surface (CLI, HTTP API, dashboard) returns this and never
/// [`Entry`], so there is no code path where a listing accidentally serialises
/// a value.
#[derive(Debug, Clone, Serialize)]
pub struct EntryInfo {
    pub namespace: String,
    pub key: String,
    pub placeholder: String,
    pub fingerprint: String,
    pub bytes: usize,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub note: Option<String>,
}

pub type Store = BTreeMap<String, BTreeMap<String, Entry>>;

/// A place secrets can be read from and written to.
///
/// Implementations are expected to be cheap to read (the broker resolves a
/// reference on every upstream request) and are free to be expensive to write.
pub trait SecretBackend: Send + Sync {
    fn get(&self, r: &SecretRef) -> Option<&Secret>;
    fn put(&mut self, r: &SecretRef, entry: Entry);
    fn delete(&mut self, r: &SecretRef) -> bool;
    fn namespaces(&self) -> Vec<String>;
    fn list(&self) -> Vec<EntryInfo>;
    /// Flush to durable storage. A backend with no local state may no-op.
    fn persist(&self) -> Result<(), VaultError>;
}

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("vault file not found at {0}")]
    NotFound(std::path::PathBuf),
    #[error("vault file at {0} already exists, refusing to overwrite it")]
    AlreadyExists(std::path::PathBuf),
    #[error("wrong passphrase, or the vault file is corrupt")]
    Decrypt,
    #[error("unsupported vault format version {0}, this build understands 1")]
    Version(u32),
    #[error("`{0}` is not a valid secret reference, expected `namespace.key`")]
    BadRef(String),
    #[error("no secret at `{0}`")]
    Missing(SecretRef),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("vault file is not valid JSON: {0}")]
    Format(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_parse_on_the_first_dot() {
        let r = SecretRef::parse("anthropic.oauth.refresh_token").unwrap();
        assert_eq!(r.namespace, "anthropic");
        assert_eq!(r.key, "oauth.refresh_token");
    }

    #[test]
    fn refs_reject_the_malformed() {
        assert!(SecretRef::parse("nodot").is_err());
        assert!(SecretRef::parse(".key").is_err());
        assert!(SecretRef::parse("ns.").is_err());
    }

    #[test]
    fn placeholders_are_loud_and_grep_safe() {
        let r = SecretRef::new("example-reseller", "api_key");
        assert_eq!(r.placeholder(), "__DOUANE_XCODE_BEST_API_KEY__");
    }
}
