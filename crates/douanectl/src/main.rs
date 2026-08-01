//! `douanectl`: the one place a human handles a credential value.
//!
//! Everything else in douane deals in references and placeholders. This binary
//! exists so the values can get into the vault once, and be rotated later,
//! without any agent session ever holding one.
//!
//! Design rules that the commands below all obey:
//!
//! - A value is never taken as a command line argument. Arguments land in shell
//!   history, in `ps` output, and in the audit log of whatever ran the command.
//!   Values come from a prompt or from stdin, never from `argv`.
//! - No command prints a value. `get` deliberately does not exist. What the
//!   broker needs is a placeholder, and `list` hands out placeholders.
//! - The passphrase is read from the terminal, or from `DOUANE_PASSPHRASE` when
//!   there is no terminal, which is how the broker's service unit supplies it.

use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use douane_vault::{Entry, FileBackend, Secret, SecretBackend, SecretRef};

#[derive(Parser)]
#[command(name = "douanectl", version, about, long_about = None)]
struct Cli {
    /// Vault file. Defaults to `$DOUANE_VAULT`, then to the platform config dir.
    #[arg(long, global = true)]
    vault: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create an empty vault.
    Init,

    /// Store a value under `namespace.key`. Reads the value from stdin or a prompt.
    Put {
        /// `namespace.key`, for instance `proxy.password`.
        reference: String,
        /// Free-text note. Never put a value in here.
        #[arg(long)]
        note: Option<String>,
    },

    /// List what the vault holds: names, placeholders, fingerprints. No values.
    List {
        #[arg(long)]
        json: bool,
    },

    /// Namespaces only. The cheapest answer to "does a credential for X exist".
    Namespaces,

    /// Remove a secret.
    Rm { reference: String },

    /// Print the placeholder for a reference, without touching the vault.
    Placeholder { reference: String },

    /// Import a flat `{"ns": {"key": "value"}}` JSON file, the shape the brain's
    /// `.secrets/credentials.json` already uses. Existing keys are kept unless
    /// `--overwrite` is passed, so a re-run is safe.
    Import {
        path: PathBuf,
        #[arg(long)]
        overwrite: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = vault_path(cli.vault)?;

    match cli.command {
        Command::Placeholder { reference } => {
            // Deliberately before any vault access: knowing the placeholder for
            // a reference needs no passphrase and no file.
            let r = SecretRef::parse(&reference).map_err(anyhow::Error::from)?;
            println!("{}", r.placeholder());
            return Ok(());
        }
        Command::Init => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
                restrict_dir(parent)?;
            }
            let passphrase = read_passphrase_twice()?;
            let vault = FileBackend::create(&path, &passphrase)?;
            eprintln!("vault created at {}", vault.path().display());
            eprintln!("nothing in it yet: `douanectl put <namespace>.<key>`");
        }
        Command::Put { reference, note } => {
            let r = SecretRef::parse(&reference).map_err(anyhow::Error::from)?;
            let passphrase = read_passphrase()?;
            let mut vault = FileBackend::open(&path, &passphrase)?;

            let value = read_value(&r)?;
            if value.is_empty() {
                bail!("refusing to store an empty value for {r}");
            }
            let mut entry = Entry::new(value);
            if let Some(note) = note {
                entry = entry.with_note(note);
            }
            let fingerprint = entry.value.fingerprint();
            vault.put(&r, entry);
            vault.persist()?;

            // The fingerprint is the receipt: it proves the write landed and
            // lets the next rotation be told apart from a no-op, and it reveals
            // nothing about the value.
            eprintln!("stored {r}  fingerprint {fingerprint}");
            eprintln!("placeholder: {}", r.placeholder());
        }
        Command::List { json } => {
            let passphrase = read_passphrase()?;
            let vault = FileBackend::open(&path, &passphrase)?;
            let info = vault.list();
            if json {
                println!("{}", serde_json::to_string_pretty(&info)?);
            } else if info.is_empty() {
                eprintln!("vault is empty");
            } else {
                for e in &info {
                    println!(
                        "{:<28} {:<10} {:>5} B  {}",
                        format!("{}.{}", e.namespace, e.key),
                        e.fingerprint,
                        e.bytes,
                        e.placeholder
                    );
                }
            }
        }
        Command::Namespaces => {
            let passphrase = read_passphrase()?;
            let vault = FileBackend::open(&path, &passphrase)?;
            for ns in vault.namespaces() {
                println!("{ns}");
            }
        }
        Command::Rm { reference } => {
            let r = SecretRef::parse(&reference).map_err(anyhow::Error::from)?;
            let passphrase = read_passphrase()?;
            let mut vault = FileBackend::open(&path, &passphrase)?;
            if !vault.delete(&r) {
                bail!("no secret at {r}");
            }
            vault.persist()?;
            eprintln!("removed {r}");
        }
        Command::Import {
            path: src,
            overwrite,
        } => {
            let passphrase = read_passphrase()?;
            let mut vault = FileBackend::open(&path, &passphrase)?;
            let (added, skipped) = import(&mut vault, &src, overwrite)?;
            vault.persist()?;
            eprintln!("imported {added} secret(s), skipped {skipped} already present");
            if skipped > 0 && !overwrite {
                eprintln!("pass --overwrite to replace what is already stored");
            }
        }
    }

    Ok(())
}

/// Read `{"namespace": {"key": "value"}}`, ignoring anything that is not a
/// string leaf. Nested objects beyond two levels are flattened with a dotted
/// key, because the brain's file has a few of those and dropping them silently
/// would be worse than a slightly odd key name.
fn import(vault: &mut FileBackend, src: &PathBuf, overwrite: bool) -> Result<(usize, usize)> {
    let raw: serde_json::Value = serde_json::from_slice(
        &std::fs::read(src).with_context(|| format!("reading {}", src.display()))?,
    )?;
    let obj = raw
        .as_object()
        .context("expected a JSON object at the top level")?;

    let mut added = 0usize;
    let mut skipped = 0usize;

    for (namespace, body) in obj {
        let Some(keys) = body.as_object() else {
            continue;
        };
        for (key, value) in flatten(keys) {
            let r = SecretRef::new(namespace.clone(), key);
            if vault.get(&r).is_some() && !overwrite {
                skipped += 1;
                continue;
            }
            vault.put(&r, Entry::new(Secret::new(value)));
            added += 1;
        }
    }

    Ok((added, skipped))
}

fn flatten(obj: &serde_json::Map<String, serde_json::Value>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (k, v) in obj {
        match v {
            serde_json::Value::String(s) => out.push((k.clone(), s.clone())),
            serde_json::Value::Object(inner) => {
                for (ik, iv) in flatten(inner) {
                    out.push((format!("{k}.{ik}"), iv));
                }
            }
            // Numbers and booleans are configuration, not credentials. A port
            // number in the vault is noise that makes the real secrets harder
            // to see.
            _ => {}
        }
    }
    out
}

fn vault_path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    if let Ok(p) = std::env::var("DOUANE_VAULT") {
        return Ok(PathBuf::from(p));
    }
    let base = if cfg!(windows) {
        std::env::var("APPDATA").map(PathBuf::from).ok()
    } else {
        std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .ok()
            .or_else(|| {
                std::env::var("HOME")
                    .map(|h| PathBuf::from(h).join(".config"))
                    .ok()
            })
    };
    let base = base.context("cannot determine a config directory, pass --vault")?;
    Ok(base.join("douane").join("vault.json"))
}

/// Ask once. Used by every command that opens an existing vault.
fn read_passphrase() -> Result<String> {
    if let Ok(p) = std::env::var("DOUANE_PASSPHRASE") {
        return Ok(p);
    }
    if !std::io::stdin().is_terminal() {
        bail!("no terminal to prompt on, set DOUANE_PASSPHRASE");
    }
    Ok(rpassword::prompt_password("vault passphrase: ")?)
}

/// Ask twice, for a vault being created. A typo here is unrecoverable: there is
/// no reset, and every secret is behind it.
fn read_passphrase_twice() -> Result<String> {
    if let Ok(p) = std::env::var("DOUANE_PASSPHRASE") {
        return Ok(p);
    }
    let a = rpassword::prompt_password("new vault passphrase: ")?;
    if a.len() < 12 {
        bail!("passphrase must be at least 12 characters, this one guards everything");
    }
    let b = rpassword::prompt_password("again: ")?;
    if a != b {
        bail!("the two passphrases differ");
    }
    Ok(a)
}

/// Take the value from stdin when piped, otherwise prompt without echo.
///
/// The piped path is what makes `douanectl put x.y < file` work for a key that
/// arrived as a file, without it ever passing through a shell argument.
fn read_value(r: &SecretRef) -> Result<Secret> {
    if !std::io::stdin().is_terminal() {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        return Ok(Secret::new(buf.trim_end_matches(['\n', '\r'])));
    }
    let mut err = std::io::stderr();
    write!(err, "value for {r} (not echoed): ")?;
    err.flush()?;
    Ok(Secret::new(rpassword::read_password()?))
}

#[cfg(unix)]
fn restrict_dir(p: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_dir(_p: &std::path::Path) -> Result<()> {
    Ok(())
}
