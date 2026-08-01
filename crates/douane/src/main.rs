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

mod audit;
mod config;
mod proxy;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::routing::{get, post};
use clap::Parser;
use douane_vault::{FileBackend, SecretBackend};

use crate::audit::Audit;
use crate::config::Config;
use crate::proxy::AppState;

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

    let config = Config::load(&cli.config)
        .with_context(|| format!("loading {}", cli.config.display()))?;

    let vault_path = cli
        .vault
        .or_else(|| std::env::var("DOUANE_VAULT").ok().map(PathBuf::from))
        .context("no vault path: pass --vault or set DOUANE_VAULT")?;

    let passphrase =
        std::env::var("DOUANE_PASSPHRASE").context("DOUANE_PASSPHRASE is not set")?;
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
    if !missing.is_empty() {
        for m in &missing {
            tracing::error!("{m}");
        }
        bail!(
            "{} vault entr{} missing, store them with `douanectl put`",
            missing.len(),
            if missing.len() == 1 { "y is" } else { "ies are" }
        );
    }

    tracing::info!(
        providers = config.providers.len(),
        routes = config.routes.len(),
        clients = config.clients.len(),
        secrets = vault.len(),
        "configuration and vault agree"
    );

    if cli.check {
        println!("ok");
        return Ok(());
    }

    let listen = config.server.listen.clone();
    let audit = Audit::open(&config.server.database)
        .with_context(|| format!("opening {}", config.server.database))?;

    let state = Arc::new(AppState {
        config,
        vault: Box::new(vault),
        // No proxy is inherited from the environment: an agent that manages to
        // set `HTTPS_PROXY` in the broker's environment must not get to redirect
        // upstream traffic through something it controls.
        http: reqwest::Client::builder()
            .no_proxy()
            .user_agent(concat!("douane/", env!("CARGO_PKG_VERSION")))
            .build()?,
        audit,
    });

    let app = Router::new()
        .route("/v1/messages", post(proxy::messages))
        .route("/health", get(proxy::health))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(64 * 1024 * 1024))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state);

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
