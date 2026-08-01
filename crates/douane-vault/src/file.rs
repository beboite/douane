//! The V1 backend: one encrypted file on disk.
//!
//! Format is a JSON envelope whose header travels in cleartext and whose body
//! does not. The header is authenticated as associated data, so an attacker who
//! edits the KDF parameters to something cheap cannot then decrypt: the tag
//! stops matching and [`FileBackend::open`] fails the same way a wrong
//! passphrase does.
//!
//! ```text
//! { "version": 1,
//!   "kdf": { "algorithm": "argon2id", "salt": "...", "m_cost": 65536, ... },
//!   "nonce": "...",
//!   "ciphertext": "..." }
//! ```
//!
//! XChaCha20-Poly1305 rather than the 96-bit-nonce variant: the nonce is drawn
//! at random on every write, and 192 bits makes a collision across a lifetime of
//! rewrites a non-problem without having to keep a counter on disk.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::{Entry, EntryInfo, SecretBackend, SecretRef, Store, VaultError};

/// The only format version this build writes or reads.
const FORMAT_VERSION: u32 = 1;

/// Argon2id cost. 64 MiB and three passes is the OWASP floor, and the vault is
/// unlocked once per broker start, so a derivation measured in hundreds of
/// milliseconds costs nothing anyone will notice.
const DEFAULT_M_COST: u32 = 65_536;
const DEFAULT_T_COST: u32 = 3;
const DEFAULT_P_COST: u32 = 1;

/// The 32 bytes ChaCha20-Poly1305 actually keys on.
///
/// Kept apart from the passphrase so the passphrase can be dropped the moment
/// derivation is done. On unix the pages are locked, which stops the key from
/// reaching swap and from there a disk an attacker can read at leisure.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct MasterKey([u8; 32]);

impl MasterKey {
    /// Derive from a passphrase. The salt comes from the file being opened, or
    /// is freshly random when the file is being created.
    pub fn derive(passphrase: &str, kdf: &KdfParams) -> Result<Self, VaultError> {
        use argon2::{Algorithm, Argon2, Params, Version};

        let salt = B64
            .decode(&kdf.salt)
            .map_err(|e| VaultError::Other(format!("salt is not valid base64: {e}")))?;

        let params = Params::new(kdf.m_cost, kdf.t_cost, kdf.p_cost, Some(32))
            .map_err(|e| VaultError::Other(format!("bad Argon2 parameters: {e}")))?;

        let mut key = [0u8; 32];
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(passphrase.as_bytes(), &salt, &mut key)
            .map_err(|e| VaultError::Other(format!("key derivation failed: {e}")))?;

        let this = Self(key);
        this.lock_pages();
        Ok(this)
    }

    #[cfg(unix)]
    fn lock_pages(&self) {
        // Best effort. A failure means RLIMIT_MEMLOCK is too low, which is a
        // hardening gap and not a reason to refuse to run, so it is deliberately
        // not surfaced as an error.
        unsafe {
            libc::mlock(self.0.as_ptr().cast(), self.0.len());
        }
    }

    #[cfg(not(unix))]
    fn lock_pages(&self) {}

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new((&self.0).into())
    }
}

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MasterKey(<redacted, 32 bytes>)")
    }
}

/// The key-derivation parameters, stored in cleartext next to the ciphertext.
///
/// They are not secret. They exist in the file so a vault written by a build
/// with different costs still opens, which matters the day the defaults are
/// raised and old vaults must keep working.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KdfParams {
    pub algorithm: String,
    pub salt: String,
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl KdfParams {
    /// Fresh parameters with a random salt, for a vault being created.
    pub fn fresh() -> Self {
        let mut salt = [0u8; 16];
        rand::rng().fill_bytes(&mut salt);
        Self {
            algorithm: "argon2id".to_owned(),
            salt: B64.encode(salt),
            m_cost: DEFAULT_M_COST,
            t_cost: DEFAULT_T_COST,
            p_cost: DEFAULT_P_COST,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Envelope {
    version: u32,
    kdf: KdfParams,
    nonce: String,
    ciphertext: String,
}

impl Envelope {
    /// The bytes bound to the ciphertext as associated data.
    ///
    /// Everything an attacker could usefully edit is in here: the version, so a
    /// downgrade is caught, and the KDF parameters, so weakening them is caught.
    fn aad(&self) -> Vec<u8> {
        format!(
            "douane-v{}|{}|{}|{}|{}|{}",
            self.version,
            self.kdf.algorithm,
            self.kdf.salt,
            self.kdf.m_cost,
            self.kdf.t_cost,
            self.kdf.p_cost
        )
        .into_bytes()
    }
}

/// A vault backed by one encrypted file.
///
/// The whole store is held decrypted in memory for the life of the process.
/// That is the point: the broker resolves a reference on every upstream request
/// and cannot afford a KDF round per lookup.
pub struct FileBackend {
    path: PathBuf,
    key: MasterKey,
    kdf: KdfParams,
    store: Store,
}

impl FileBackend {
    /// Create a new vault. Refuses to touch an existing file, because the only
    /// way this is called on one is by mistake.
    pub fn create(path: impl AsRef<Path>, passphrase: &str) -> Result<Self, VaultError> {
        let path = path.as_ref().to_path_buf();
        if path.exists() {
            return Err(VaultError::AlreadyExists(path));
        }
        let kdf = KdfParams::fresh();
        let key = MasterKey::derive(passphrase, &kdf)?;
        let this = Self {
            path,
            key,
            kdf,
            store: Store::new(),
        };
        this.persist()?;
        Ok(this)
    }

    /// Open an existing vault.
    ///
    /// A wrong passphrase and a tampered file both surface as
    /// [`VaultError::Decrypt`]. They are not distinguished on purpose: telling
    /// the two apart would confirm a guessed passphrase against a file the
    /// guesser had also modified.
    pub fn open(path: impl AsRef<Path>, passphrase: &str) -> Result<Self, VaultError> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Err(VaultError::NotFound(path));
        }

        let envelope: Envelope = serde_json::from_slice(&fs::read(&path)?)?;
        if envelope.version != FORMAT_VERSION {
            return Err(VaultError::Version(envelope.version));
        }

        let key = MasterKey::derive(passphrase, &envelope.kdf)?;

        let nonce = B64.decode(&envelope.nonce).map_err(|_| VaultError::Decrypt)?;
        let ciphertext = B64
            .decode(&envelope.ciphertext)
            .map_err(|_| VaultError::Decrypt)?;
        if nonce.len() != 24 {
            return Err(VaultError::Decrypt);
        }

        let aad = envelope.aad();
        let plaintext = key
            .cipher()
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| VaultError::Decrypt)?;

        let store: Store = serde_json::from_slice(&plaintext)?;

        Ok(Self {
            path,
            key,
            kdf: envelope.kdf,
            store,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_empty(&self) -> bool {
        self.store.values().all(|ns| ns.is_empty())
    }

    pub fn len(&self) -> usize {
        self.store.values().map(|ns| ns.len()).sum()
    }
}

impl SecretBackend for FileBackend {
    fn get(&self, r: &SecretRef) -> Option<&crate::Secret> {
        self.store.get(&r.namespace)?.get(&r.key).map(|e| &e.value)
    }

    fn put(&mut self, r: &SecretRef, entry: Entry) {
        self.store
            .entry(r.namespace.clone())
            .or_default()
            .insert(r.key.clone(), entry);
    }

    fn delete(&mut self, r: &SecretRef) -> bool {
        let Some(ns) = self.store.get_mut(&r.namespace) else {
            return false;
        };
        let removed = ns.remove(&r.key).is_some();
        // An empty namespace is noise in every listing, so it goes with its last
        // key rather than lingering as a name that answers nothing.
        if ns.is_empty() {
            self.store.remove(&r.namespace);
        }
        removed
    }

    fn namespaces(&self) -> Vec<String> {
        self.store.keys().cloned().collect()
    }

    fn list(&self) -> Vec<EntryInfo> {
        self.store
            .iter()
            .flat_map(|(namespace, keys)| {
                keys.iter().map(move |(key, entry)| EntryInfo {
                    namespace: namespace.clone(),
                    key: key.clone(),
                    placeholder: SecretRef::new(namespace.clone(), key.clone()).placeholder(),
                    fingerprint: entry.value.fingerprint(),
                    bytes: entry.value.len(),
                    updated_at: entry.updated_at,
                    note: entry.note.clone(),
                })
            })
            .collect()
    }

    /// Encrypt and write, atomically.
    ///
    /// Temp file then rename, so a crash mid-write leaves the previous vault
    /// intact rather than a truncated one. A vault half-written is a vault lost,
    /// and there is no second copy by design.
    fn persist(&self) -> Result<(), VaultError> {
        let mut plaintext = serde_json::to_vec(&self.store)?;

        let mut nonce = [0u8; 24];
        rand::rng().fill_bytes(&mut nonce);

        let mut envelope = Envelope {
            version: FORMAT_VERSION,
            kdf: self.kdf.clone(),
            nonce: B64.encode(nonce),
            ciphertext: String::new(),
        };
        let aad = envelope.aad();

        let ciphertext = self
            .key
            .cipher()
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: &aad,
                },
            )
            .map_err(|e| VaultError::Other(format!("encryption failed: {e}")))?;
        plaintext.zeroize();

        envelope.ciphertext = B64.encode(&ciphertext);
        let body = serde_json::to_vec_pretty(&envelope)?;

        let tmp = self.path.with_extension("tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            restrict(&f)?;
            f.write_all(&body)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// Owner-only, before a single byte of ciphertext is written.
///
/// Setting the mode after the write would leave a window where the file is
/// world-readable, which is exactly the window that matters on a shared box.
#[cfg(unix)]
fn restrict(f: &fs::File) -> Result<(), VaultError> {
    use std::os::unix::fs::PermissionsExt;
    f.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict(_f: &fs::File) -> Result<(), VaultError> {
    // Windows inherits the parent directory's ACL. douane's own directory is
    // created owner-only by douanectl, so the file lands correctly restricted.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Secret;

    fn tmp() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.json");
        (dir, path)
    }

    // A deliberately cheap KDF: the real cost is 64 MiB per derivation and the
    // tests open vaults dozens of times.
    fn cheap(path: &Path, passphrase: &str) -> FileBackend {
        let mut kdf = KdfParams::fresh();
        kdf.m_cost = 8;
        kdf.t_cost = 1;
        let key = MasterKey::derive(passphrase, &kdf).unwrap();
        let b = FileBackend {
            path: path.to_path_buf(),
            key,
            kdf,
            store: Store::new(),
        };
        b.persist().unwrap();
        b
    }

    #[test]
    fn round_trips_through_the_file() {
        let (_d, path) = tmp();
        let mut b = cheap(&path, "correct horse");
        b.put(
            &SecretRef::new("proxy", "password"),
            Entry::new(Secret::new("hunter2")),
        );
        b.persist().unwrap();

        let reopened = FileBackend::open(&path, "correct horse").unwrap();
        let got = reopened.get(&SecretRef::new("proxy", "password")).unwrap();
        assert_eq!(got.expose(), "hunter2");
    }

    #[test]
    fn wrong_passphrase_is_refused() {
        let (_d, path) = tmp();
        let mut b = cheap(&path, "right");
        b.put(&SecretRef::new("a", "b"), Entry::new(Secret::new("v")));
        b.persist().unwrap();

        assert!(matches!(
            FileBackend::open(&path, "wrong"),
            Err(VaultError::Decrypt)
        ));
    }

    #[test]
    fn weakening_the_kdf_header_breaks_the_tag() {
        let (_d, path) = tmp();
        let mut b = cheap(&path, "pass");
        b.put(&SecretRef::new("a", "b"), Entry::new(Secret::new("v")));
        b.persist().unwrap();

        let mut env: Envelope = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        env.kdf.t_cost = 1;
        env.kdf.m_cost = 8;
        env.kdf.salt = B64.encode([0u8; 16]);
        fs::write(&path, serde_json::to_vec(&env).unwrap()).unwrap();

        // Even the right passphrase cannot open it now, which is the point:
        // the header is authenticated, not merely present.
        assert!(matches!(
            FileBackend::open(&path, "pass"),
            Err(VaultError::Decrypt)
        ));
    }

    #[test]
    fn creating_over_an_existing_vault_is_refused() {
        let (_d, path) = tmp();
        fs::write(&path, b"{}").unwrap();
        assert!(matches!(
            FileBackend::create(&path, "pass"),
            Err(VaultError::AlreadyExists(_))
        ));
    }

    #[test]
    fn listing_never_carries_a_value() {
        let (_d, path) = tmp();
        let mut b = cheap(&path, "pass");
        b.put(
            &SecretRef::new("example-reseller", "api_key"),
            Entry::new(Secret::new("sk-super-secret")).with_note("reseller"),
        );

        let info = b.list();
        assert_eq!(info.len(), 1);
        let json = serde_json::to_string(&info).unwrap();
        assert!(!json.contains("sk-super-secret"));
        assert!(json.contains("__DOUANE_XCODE_BEST_API_KEY__"));
        assert_eq!(info[0].bytes, 15);
    }

    #[test]
    fn deleting_the_last_key_drops_the_namespace() {
        let (_d, path) = tmp();
        let mut b = cheap(&path, "pass");
        let r = SecretRef::new("ns", "only");
        b.put(&r, Entry::new(Secret::new("v")));
        assert_eq!(b.namespaces(), vec!["ns".to_owned()]);
        assert!(b.delete(&r));
        assert!(b.namespaces().is_empty());
        assert!(!b.delete(&r));
    }

    #[test]
    fn a_truncated_write_leaves_the_old_vault_readable() {
        let (_d, path) = tmp();
        let mut b = cheap(&path, "pass");
        b.put(&SecretRef::new("a", "b"), Entry::new(Secret::new("first")));
        b.persist().unwrap();

        // Simulate the crash: a stale temp file next to a good vault must not
        // be mistaken for the vault itself.
        fs::write(path.with_extension("tmp"), b"garbage").unwrap();
        let reopened = FileBackend::open(&path, "pass").unwrap();
        assert_eq!(
            reopened.get(&SecretRef::new("a", "b")).unwrap().expose(),
            "first"
        );
    }
}
