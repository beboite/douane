//! One SQLite writer off the async runtime; bounded history reads and bounded queue.

use anyhow::Result;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};
use tokio::sync::{mpsc, oneshot};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Call {
    pub id: String,
    pub started_at: String,
    pub provider: String,
    pub account_id: String,
    pub model: String,
    pub status: u16,
    pub latency_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub error: Option<String>,
    /// The douane client that made the call; `None` for calls made before
    /// client keys were required.
    #[serde(default)]
    pub client_id: Option<String>,
}

/// What one client spent on one model of one provider over a period.
/// `client_id` is empty for calls made before keys were required.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct ClientTotals {
    pub client_id: String,
    pub provider: String,
    pub model: String,
    pub calls: u64,
    pub errors: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub latency_ms: u64,
    pub last_at: String,
}

/// One client's calls and tokens in one UTC hour, `hour` as `YYYY-MM-DDTHH`.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct ClientHour {
    pub client_id: String,
    pub hour: String,
    pub calls: u64,
    pub tokens: u64,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct ClientStats {
    pub totals: Vec<ClientTotals>,
    pub hours: Vec<ClientHour>,
}

enum Command {
    Record(Call),
    List(usize, oneshot::Sender<Vec<Call>>),
    Stats(String, oneshot::Sender<ClientStats>),
    Flush(oneshot::Sender<()>),
}

pub struct Ledger {
    tx: mpsc::Sender<Command>,
    pub requests: AtomicU64,
    pub errors: AtomicU64,
    pub input_tokens: AtomicU64,
    pub output_tokens: AtomicU64,
    pub cache_read_tokens: AtomicU64,
    pub cache_write_tokens: AtomicU64,
    pub in_flight: AtomicU64,
    pub dropped_logs: AtomicU64,
    pub started: Instant,
}

impl Ledger {
    pub fn open(path: &Path) -> Result<Arc<Self>> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let db = Connection::open(path)?;
        anyhow::ensure!(
            !db.is_readonly("main")?,
            "request database must be writable by the service user"
        );
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; CREATE TABLE IF NOT EXISTS subscription_calls(id TEXT PRIMARY KEY,started_at TEXT NOT NULL,provider TEXT NOT NULL,account_id TEXT NOT NULL,model TEXT NOT NULL,status INTEGER NOT NULL,latency_ms INTEGER NOT NULL,input_tokens INTEGER NOT NULL,output_tokens INTEGER NOT NULL,error TEXT,cache_read_tokens INTEGER NOT NULL DEFAULT 0,cache_write_tokens INTEGER NOT NULL DEFAULT 0); CREATE INDEX IF NOT EXISTS subscription_calls_time ON subscription_calls(started_at DESC);")?;
        let has_client: bool = db
            .prepare(
                "SELECT 1 FROM pragma_table_info('subscription_calls') WHERE name='client_id'",
            )?
            .exists([])?;
        if !has_client {
            db.execute_batch("ALTER TABLE subscription_calls ADD COLUMN client_id TEXT")?;
        }
        let (requests,errors,input,output,cache_read,cache_write): (u64,u64,u64,u64,u64,u64)=db.query_row("SELECT count(*),coalesce(sum(status>=400 OR error IS NOT NULL),0),coalesce(sum(input_tokens),0),coalesce(sum(output_tokens),0),coalesce(sum(cache_read_tokens),0),coalesce(sum(cache_write_tokens),0) FROM subscription_calls",[],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
        let (tx, mut rx) = mpsc::channel(1024);
        std::thread::Builder::new().name("douane-ledger".into()).spawn(move || {
            while let Some(command)=rx.blocking_recv() {
                match command {
                    Command::Record(c) => {
                        if let Err(e)=db.execute("INSERT INTO subscription_calls(id,started_at,provider,account_id,model,status,latency_ms,input_tokens,output_tokens,error,cache_read_tokens,cache_write_tokens,client_id) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",params![c.id,c.started_at,c.provider,c.account_id,c.model,c.status,c.latency_ms,c.input_tokens,c.output_tokens,c.error,c.cache_read_tokens,c.cache_write_tokens,c.client_id]) { tracing::warn!(error=%e,"could not persist request metadata"); }
                    },
                    Command::List(limit,reply) => {
                        let rows=(|| -> rusqlite::Result<Vec<Call>> {
                            let mut stmt=db.prepare("SELECT id,started_at,provider,account_id,model,status,latency_ms,input_tokens,output_tokens,error,cache_read_tokens,cache_write_tokens,client_id FROM subscription_calls ORDER BY started_at DESC LIMIT ?1")?;
                            stmt.query_map([limit as u64],|r| Ok(Call{id:r.get(0)?,started_at:r.get(1)?,provider:r.get(2)?,account_id:r.get(3)?,model:r.get(4)?,status:r.get(5)?,latency_ms:r.get(6)?,input_tokens:r.get(7)?,output_tokens:r.get(8)?,error:r.get(9)?,cache_read_tokens:r.get(10)?,cache_write_tokens:r.get(11)?,client_id:r.get(12)?}))?.collect()
                        })().unwrap_or_default();
                        let _=reply.send(rows);
                    },
                    Command::Stats(since,reply) => {
                        let _=reply.send(client_stats(&db,&since).unwrap_or_default());
                    },
                    Command::Flush(reply) => { let _=db.execute_batch("PRAGMA wal_checkpoint(PASSIVE)"); let _=reply.send(()); },
                }
            }
        })?;
        Ok(Arc::new(Self {
            tx,
            requests: AtomicU64::new(requests),
            errors: AtomicU64::new(errors),
            input_tokens: AtomicU64::new(input),
            output_tokens: AtomicU64::new(output),
            cache_read_tokens: AtomicU64::new(cache_read),
            cache_write_tokens: AtomicU64::new(cache_write),
            in_flight: AtomicU64::new(0),
            dropped_logs: AtomicU64::new(0),
            started: Instant::now(),
        }))
    }
    pub fn record(&self, c: Call) {
        self.requests.fetch_add(1, Ordering::Relaxed);
        self.errors.fetch_add(
            u64::from(c.status >= 400 || c.error.is_some()),
            Ordering::Relaxed,
        );
        self.input_tokens
            .fetch_add(c.input_tokens, Ordering::Relaxed);
        self.output_tokens
            .fetch_add(c.output_tokens, Ordering::Relaxed);
        self.cache_read_tokens
            .fetch_add(c.cache_read_tokens, Ordering::Relaxed);
        self.cache_write_tokens
            .fetch_add(c.cache_write_tokens, Ordering::Relaxed);
        if self.tx.try_send(Command::Record(c)).is_err() {
            self.dropped_logs.fetch_add(1, Ordering::Relaxed);
        }
    }
    pub async fn calls(&self, limit: usize) -> Vec<Call> {
        let (tx, rx) = oneshot::channel();
        if self
            .tx
            .send(Command::List(limit.clamp(1, 500), tx))
            .await
            .is_err()
        {
            return vec![];
        }
        rx.await.unwrap_or_default()
    }
    /// Totals by client, provider and model, and by client and hour, of the
    /// calls recorded since `since` (RFC 3339, UTC).
    pub async fn client_stats(&self, since: String) -> ClientStats {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Command::Stats(since, tx)).await.is_err() {
            return ClientStats::default();
        }
        rx.await.unwrap_or_default()
    }
    pub async fn flush(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Command::Flush(tx)).await.is_ok() {
            let _ = rx.await;
        }
    }
}

fn client_stats(db: &Connection, since: &str) -> rusqlite::Result<ClientStats> {
    let mut totals = db.prepare(
        "SELECT coalesce(client_id,''),provider,model,count(*),coalesce(sum(status>=400 OR error IS NOT NULL),0),         coalesce(sum(input_tokens),0),coalesce(sum(output_tokens),0),coalesce(sum(cache_read_tokens),0),         coalesce(sum(cache_write_tokens),0),coalesce(sum(latency_ms),0),max(started_at)          FROM subscription_calls WHERE started_at>=?1 GROUP BY 1,2,3 ORDER BY 1,2,3",
    )?;
    let totals = totals
        .query_map([since], |r| {
            Ok(ClientTotals {
                client_id: r.get(0)?,
                provider: r.get(1)?,
                model: r.get(2)?,
                calls: r.get(3)?,
                errors: r.get(4)?,
                input_tokens: r.get(5)?,
                output_tokens: r.get(6)?,
                cache_read_tokens: r.get(7)?,
                cache_write_tokens: r.get(8)?,
                latency_ms: r.get(9)?,
                last_at: r.get(10)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut hours = db.prepare(
        "SELECT coalesce(client_id,''),substr(started_at,1,13),count(*),         coalesce(sum(input_tokens+output_tokens),0)          FROM subscription_calls WHERE started_at>=?1 GROUP BY 1,2 ORDER BY 2,1",
    )?;
    let hours = hours
        .query_map([since], |r| {
            Ok(ClientHour {
                client_id: r.get(0)?,
                hour: r.get(1)?,
                calls: r.get(2)?,
                tokens: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ClientStats { totals, hours })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, at: &str, client: Option<&str>, model: &str, status: u16) -> Call {
        Call {
            id: id.into(),
            started_at: at.into(),
            provider: "claude".into(),
            account_id: "a".into(),
            model: model.into(),
            status,
            latency_ms: 100,
            input_tokens: 1000,
            output_tokens: 50,
            cache_read_tokens: 800,
            client_id: client.map(Into::into),
            ..Call::default()
        }
    }

    #[tokio::test]
    async fn client_stats_sum_each_client_by_model_and_by_hour_since_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&dir.path().join("calls.sqlite")).unwrap();
        ledger.record(call("1", "2026-10-20T08:00:00Z", Some("nuno"), "opus", 200));
        ledger.record(call("2", "2026-10-21T08:10:00Z", Some("nuno"), "opus", 200));
        ledger.record(call("3", "2026-10-21T08:50:00Z", Some("nuno"), "opus", 429));
        ledger.record(call("4", "2026-10-21T09:05:00Z", Some("nuno"), "sonnet", 200));
        ledger.record(call("5", "2026-10-21T09:06:00Z", Some("famille"), "opus", 200));
        ledger.record(call("6", "2026-10-21T09:07:00Z", None, "opus", 200));
        ledger.flush().await;
        let stats = ledger.client_stats("2026-10-21T00:00:00Z".into()).await;
        let opus = stats
            .totals
            .iter()
            .find(|t| t.client_id == "nuno" && t.model == "opus")
            .unwrap();
        assert_eq!(
            (opus.calls, opus.errors, opus.input_tokens, opus.output_tokens),
            (2, 1, 2000, 100)
        );
        assert_eq!(opus.cache_read_tokens, 1600);
        assert_eq!(opus.last_at, "2026-10-21T08:50:00Z");
        assert_eq!(stats.totals.len(), 4);
        assert!(stats.totals.iter().any(|t| t.client_id.is_empty()));
        let nuno: Vec<_> = stats
            .hours
            .iter()
            .filter(|h| h.client_id == "nuno")
            .map(|h| (h.hour.as_str(), h.calls, h.tokens))
            .collect();
        assert_eq!(
            nuno,
            [("2026-10-21T08", 2, 2100), ("2026-10-21T09", 1, 1050)]
        );
    }
}
