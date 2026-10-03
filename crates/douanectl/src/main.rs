use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Manage subscription accounts without printing credentials"
)]
struct Cli {
    #[arg(long, global = true, default_value = "http://127.0.0.1:8787")]
    url: String,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Status,
    Accounts,
    Models,
    Quotas {
        #[arg(long)]
        refresh: bool,
    },
    Import {
        path: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    Remove {
        id: String,
    },
    Enable {
        id: String,
    },
    Disable {
        id: String,
    },
    Login {
        provider: String,
    },
    Complete {
        id: String,
        #[arg(long)]
        callback_file: PathBuf,
    },
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;
    let root = cli.url.trim_end_matches('/');
    let mut request = match cli.command {
        Command::Status => client.get(format!("{root}/admin/api/summary")),
        Command::Accounts => client.get(format!("{root}/admin/api/accounts")),
        Command::Models => client.get(format!("{root}/admin/api/models")),
        Command::Quotas { refresh: false } => client.get(format!("{root}/admin/api/quotas")),
        Command::Quotas { refresh: true } => client
            .post(format!("{root}/admin/api/quotas/refresh"))
            .json(&json!({})),
        Command::Import { path, name } => {
            let content: Value =
                serde_json::from_slice(&tokio::fs::read(&path).await.context("reading auth file")?)
                    .context("invalid auth JSON")?;
            client
                .post(format!("{root}/admin/api/accounts/import"))
                .json(&json!({"files":[{"name":name,"content":content}]}))
        }
        Command::Remove { id } => client.delete(format!("{root}/admin/api/accounts/{id}")),
        Command::Enable { id } => client
            .patch(format!("{root}/admin/api/accounts/{id}"))
            .json(&json!({"enabled":true})),
        Command::Disable { id } => client
            .patch(format!("{root}/admin/api/accounts/{id}"))
            .json(&json!({"enabled":false})),
        Command::Login { provider } => client
            .post(format!("{root}/admin/api/oauth/start"))
            .json(&json!({"provider":provider})),
        Command::Complete { id, callback_file } => {
            let callback = tokio::fs::read_to_string(callback_file).await?;
            client
                .post(format!("{root}/admin/api/oauth/callback"))
                .json(&json!({"id":id,"code":callback.trim(),"state":""}))
        }
    };
    if let Ok(token) = std::env::var("DOUANE_MANAGEMENT_TOKEN") {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.context("cannot reach douane")?;
    let status = response.status();
    let value: Value = response.json().await.context("invalid server response")?;
    if !status.is_success() {
        bail!(
            "douane returned {status}: {}",
            value["error"]["message"]
                .as_str()
                .or_else(|| value.pointer("/errors/0/error").and_then(Value::as_str))
                .unwrap_or("request failed")
        );
    }
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
