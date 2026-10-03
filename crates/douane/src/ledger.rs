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
}

enum Command {
    Record(Call),
    List(usize, oneshot::Sender<Vec<Call>>),
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
        let (requests,errors,input,output,cache_read,cache_write): (u64,u64,u64,u64,u64,u64)=db.query_row("SELECT count(*),coalesce(sum(status>=400 OR error IS NOT NULL),0),coalesce(sum(input_tokens),0),coalesce(sum(output_tokens),0),coalesce(sum(cache_read_tokens),0),coalesce(sum(cache_write_tokens),0) FROM subscription_calls",[],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
        let (tx, mut rx) = mpsc::channel(1024);
        std::thread::Builder::new().name("douane-ledger".into()).spawn(move || {
            while let Some(command)=rx.blocking_recv() {
                match command {
                    Command::Record(c) => {
                        if let Err(e)=db.execute("INSERT INTO subscription_calls VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",params![c.id,c.started_at,c.provider,c.account_id,c.model,c.status,c.latency_ms,c.input_tokens,c.output_tokens,c.error,c.cache_read_tokens,c.cache_write_tokens]) { tracing::warn!(error=%e,"could not persist request metadata"); }
                    },
                    Command::List(limit,reply) => {
                        let rows=(|| -> rusqlite::Result<Vec<Call>> {
                            let mut stmt=db.prepare("SELECT id,started_at,provider,account_id,model,status,latency_ms,input_tokens,output_tokens,error,cache_read_tokens,cache_write_tokens FROM subscription_calls ORDER BY started_at DESC LIMIT ?1")?;
                            stmt.query_map([limit as u64],|r| Ok(Call{id:r.get(0)?,started_at:r.get(1)?,provider:r.get(2)?,account_id:r.get(3)?,model:r.get(4)?,status:r.get(5)?,latency_ms:r.get(6)?,input_tokens:r.get(7)?,output_tokens:r.get(8)?,error:r.get(9)?,cache_read_tokens:r.get(10)?,cache_write_tokens:r.get(11)?}))?.collect()
                        })().unwrap_or_default();
                        let _=reply.send(rows);
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
    pub async fn flush(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Command::Flush(tx)).await.is_ok() {
            let _ = rx.await;
        }
    }
}
