//! The douane broker.
//!
//! Runs as a service. Reads its configuration, unlocks the vault once at
//! startup, and from then on answers Anthropic Messages requests from agents
//! while doing the talking to real providers itself.
//!
//! The passphrase arrives in `DOUANE_PASSPHRASE`, which the service unit is
//! expected to supply from a file only root can read. It is read once and the
//! variable is cleared straight after, so a later `/proc/self/environ` read by
//! anything that manages to run in this process finds nothing.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::Parser;
use douane::audit::Audit;
use douane::config::Config;
use douane::proxy::AppState;
use douane_vault::{FileBackend, SecretBackend};

#[derive(Parser)]
#[command(name = "douane", version, about, long_about = None)]
struct Cli {
    /// Broker configuration. Holds no secrets, only references into the vault.
    #[arg(long, default_value = "douane.toml")]
    config: PathBuf,

    /// Vault file. Defaults to `$DOUANE_VAULT`.
    #[arg(long)]
    vault: Option<PathBuf>,

    /// Load and validate everything, then exit without listening. What CI runs,
    /// and what to run by hand before restarting the service.
    #[arg(long)]
    check: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "douane=info,tower_http=warn".into()),
        )
        .init();

    let cli = Cli::parse();

    let config =
        Config::load(&cli.config).with_context(|| format!("loading {}", cli.config.display()))?;

    let vault_path = cli
        .vault
        .or_else(|| std::env::var("DOUANE_VAULT").ok().map(PathBuf::from))
        .context("no vault path: pass --vault or set DOUANE_VAULT")?;

    let passphrase = std::env::var("DOUANE_PASSPHRASE").context("DOUANE_PASSPHRASE is not set")?;
    // SAFETY: single-threaded at this point, nothing else can be reading the
    // environment. Clearing it shrinks the window in which the passphrase is
    // readable from outside the process to the startup path alone.
    unsafe { std::env::remove_var("DOUANE_PASSPHRASE") };

    let vault = FileBackend::open(&vault_path, &passphrase)
        .with_context(|| format!("opening vault {}", vault_path.display()))?;
    drop(passphrase);

    // Fail at startup rather than on the first request that needs a key which
    // was never stored. A broker that starts is a broker that can serve.
    let mut missing = Vec::new();
    for (name, provider) in &config.providers {
        if let Some(r) = &provider.key
            && vault.get(r).is_none()
        {
            missing.push(format!("provider `{name}` needs {r}"));
        }
    }
    for (name, client) in &config.clients {
        if vault.get(&client.key).is_none() {
            missing.push(format!("client `{name}` needs {}", client.key));
        }
    }
    if let Some(r) = &config.server.admin_key
        && vault.get(r).is_none()
    {
        missing.push(format!("the dashboard needs {r}"));
    }
    if !missing.is_empty() {
        for m in &missing {
            tracing::error!("{m}");
        }
        bail!(
            "{} vault entr{} missing, store them with `douanectl put`",
            missing.len(),
            if missing.len() == 1 {
                "y is"
            } else {
                "ies are"
            }
        );
    }

    // Confirm the upstreams that are meant to be on the internet actually are.
    // On a network whose resolver carries a search domain over a wildcard, a
    // lapsed domain does not fail: it answers with a LAN address, and the real
    // API key would then be posted to whatever is listening there.
    let mut fatal = 0;
    for p in douane::guard::check_providers(&config).await {
        if p.keyed {
            fatal += 1;
            tracing::error!("provider `{}`: {}", p.provider, p.detail);
        } else {
            tracing::warn!("provider `{}`: {}", p.provider, p.detail);
        }
    }
    if fatal > 0 {
        bail!(
            "{fatal} provider(s) hold a credential and resolve onto this network. \
             Refusing to start rather than hand a key to whatever answers there."
        );
    }

    tracing::info!(
        providers = config.providers.len(),
        routes = config.routes.len(),
        clients = config.clients.len(),
        secrets = vault.len(),
        dashboard = config.server.admin_key.is_some(),
        "configuration and vault agree"
    );

    if cli.check {
        println!("ok");
        return Ok(());
    }

    let listen = config.server.listen.clone();
    let audit = Audit::open(&config.server.database)
        .with_context(|| format!("opening {}", config.server.database))?;

    let state = Arc::new(AppState::new(
        config,
        Box::new(vault),
        // No proxy is inherited from the environment: an agent that manages to
        // set `HTTPS_PROXY` in the broker's environment must not get to redirect
        // upstream traffic through something it controls.
        //
        // `read_timeout` rather than a total one: it fires on an upstream that
        // has gone silent, and leaves a long generation alone.
        reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(std::time::Duration::from_secs(10))
            .read_timeout(std::time::Duration::from_secs(180))
            .user_agent(concat!("douane/", env!("CARGO_PKG_VERSION")))
            .build()?,
        audit,
    )?);

    tracing::info!(
        scrub_watching = state.scrubber().watching(),
        "the outbound scrubber knows what to look for"
    );

    let app = douane::router(state);

    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    tracing::info!("listening on {listen}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
