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

/// Where the egress broker's signing key lives. One name, used by the tool that
/// mints it and by the broker that loads it, so the two cannot disagree.
const CA_KEY: &str = "douane.ca_key";

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

    /// Run a command with an approved credential in its environment.
    ///
    /// The value comes from the broker, not from a local vault: it moves only
    /// after the owner approved this machine's request for it, only until that
    /// approval lapses, and it lands in the child process's environment, never
    /// in `argv` and never on stdout. The variable is named after the
    /// reference: `proxy.password` becomes `DOUANE_PROXY_PASSWORD`.
    Use {
        /// `namespace.key`, the reference an approved grant covers.
        reference: String,
        /// Broker base URL. Defaults to `$DOUANE_BROKER`.
        #[arg(long)]
        broker: Option<String>,
        /// File holding this machine's virtual key. Defaults to
        /// `$DOUANE_VK_FILE`, then to the `$DOUANE_VK` value itself.
        #[arg(long)]
        key_file: Option<PathBuf>,
        /// The command to run, after `--`.
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },

    /// The egress broker's certificate authority.
    #[command(subcommand)]
    Ca(CaCommand),
}

#[derive(Subcommand)]
enum CaCommand {
    /// Mint a new authority. The private key goes into the vault, the
    /// certificate is written where `--cert` says.
    ///
    /// The certificate is public and has to be, since it is the thing a machine
    /// is asked to trust. The key is a credential like any other and is treated
    /// like one. Refuses to overwrite an existing authority: replacing it
    /// silently would invalidate every trust store that already has the old one
    /// while looking like nothing happened.
    Init {
        /// Where to write the public certificate.
        #[arg(long, default_value = "douane-ca.crt")]
        cert: PathBuf,
        /// What the authority calls itself, as it will appear in a trust store.
        #[arg(long, default_value = "douane egress broker")]
        name: String,
        /// Replace an existing key. Every machine trusting the old certificate
        /// stops trusting this instance until it is given the new one.
        #[arg(long)]
        rotate: bool,
    },
}

/// This machine's virtual key: from the named file, from `$DOUANE_VK_FILE`,
/// or from `$DOUANE_VK` directly. A file beats an environment variable and
/// both beat `argv`, which is why there is no `--key <value>` flag.
fn virtual_key(key_file: Option<PathBuf>) -> Result<String> {
    if let Some(path) = key_file.or_else(|| std::env::var("DOUANE_VK_FILE").ok().map(PathBuf::from))
    {
        let key = std::fs::read_to_string(&path)
            .with_context(|| format!("reading the virtual key from {}", path.display()))?;
        return Ok(key.trim().to_owned());
    }
    std::env::var("DOUANE_VK")
        .context("no virtual key: pass --key-file, or set DOUANE_VK_FILE or DOUANE_VK")
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
        Command::Use {
            reference,
            broker,
            key_file,
            command,
        } => {
            // No vault and no passphrase on this path: the machine running an
            // agent holds a virtual key and nothing else, which is the point.
            let r = SecretRef::parse(&reference).map_err(anyhow::Error::from)?;
            let broker = broker
                .or_else(|| std::env::var("DOUANE_BROKER").ok())
                .context("no broker address: pass --broker or set DOUANE_BROKER")?;
            let key = virtual_key(key_file)?;

            let url = format!("{}/agent/api/use", broker.trim_end_matches('/'));
            let value = match ureq::post(&url)
                .set("x-api-key", &key)
                .send_json(serde_json::json!({ "reference": reference }))
            {
                Ok(res) => res
                    .into_json::<serde_json::Value>()?
                    .get("value")
                    .and_then(|v| v.as_str())
                    .context("the broker's answer carries no value")?
                    .to_owned(),
                Err(ureq::Error::Status(code, res)) => {
                    let why = res
                        .into_json::<serde_json::Value>()
                        .ok()
                        .and_then(|v| {
                            v.pointer("/error/message").and_then(|m| m.as_str()).map(str::to_owned)
                        })
                        .unwrap_or_else(|| format!("HTTP {code}"));
                    bail!("the broker refused: {why}");
                }
                Err(e) => bail!("cannot reach the broker at {url}: {e}"),
            };

            // `__DOUANE_PROXY_PASSWORD__` minus the underscores is a name a
            // shell can address, and it is the same name the placeholder
            // teaches, so nothing new to learn.
            let var = r.placeholder().trim_matches('_').to_owned();
            eprintln!("{r} is in the child's environment as ${var}, and nowhere else");

            let status = std::process::Command::new(&command[0])
                .args(&command[1..])
                .env(&var, &value)
                .status()
                .with_context(|| format!("running `{}`", command[0]))?;
            std::process::exit(status.code().unwrap_or(1));
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
        Command::Ca(CaCommand::Init { cert, name, rotate }) => {
            let key_ref = SecretRef::parse(CA_KEY).map_err(anyhow::Error::from)?;
            let passphrase = read_passphrase()?;
            let mut vault = FileBackend::open(&path, &passphrase)?;

            if vault.get(&key_ref).is_some() && !rotate {
                bail!(
                    "{key_ref} already exists. Minting another would leave every machine \
                     that trusts the current certificate unable to reach this broker. \
                     Pass --rotate if that is what you want."
                );
            }
            if cert.exists() && !rotate {
                bail!("{} already exists", cert.display());
            }

            let authority = douane_ca::Authority::generate(&name)?;
            // The certificate lands on disk first. A key in the vault whose
            // certificate was never written is an authority nobody can verify,
            // and there is deliberately no way to read the key back out to
            // rebuild one.
            std::fs::write(&cert, &authority.cert_pem)
                .with_context(|| format!("writing {}", cert.display()))?;

            let entry = Entry::new(Secret::new(authority.key_pem)).with_note(
                "egress broker signing key. Never leaves the vault, never reaches a client.",
            );
            let fingerprint = entry.value.fingerprint();
            vault.put(&key_ref, entry);
            vault.persist()?;

            eprintln!("authority minted, key stored as {key_ref}  fingerprint {fingerprint}");
            eprintln!("certificate written to {}", cert.display());
            eprintln!();
            eprintln!(
                "Nothing trusts it yet, and nothing here will install it for you. Adding a\n\
                 certificate authority to a machine means that machine will believe douane\n\
                 about every TLS connection it opens, which is a decision to take on purpose."
            );
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
