//! The request log.
//!
//! One row per upstream call. This is what makes the dashboard possible, and
//! more importantly it is what lets a provider's invoice be checked against
//! what douane actually sent. A reseller that bills for tokens nobody asked for
//! shows up here as a gap, and there is no other place that gap is visible.
//!
//! No request body is stored. Bodies are where the secrets are, and a log that
//! holds them is a second copy of everything the vault exists to protect.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;
use time::OffsetDateTime;

/// A SQLite connection is not `Sync`, and the broker's state is shared across
/// every request task, so the connection lives behind a mutex. With WAL enabled
/// an insert is a sub-millisecond operation, so the contention this introduces
/// is far cheaper than a pool would be to carry.
pub struct Audit {
    conn: Mutex<Connection>,
}

/// What is known about one upstream call once it has finished.
#[derive(Debug, Clone)]
pub struct Record {
    pub client: String,
    pub route: String,
    pub provider: String,
    pub model_requested: String,
    pub model_sent: String,
    pub trust: &'static str,
    /// Identifies the calling agent without any cooperation from it: a hash of
    /// the system prompt. Claude Code gives each subagent type its own, so this
    /// separates the investigator from the builder in the dashboard.
    pub agent_fingerprint: Option<String>,
    /// Groups calls into a conversation. The Anthropic wire replays the whole
    /// history every turn, so hashing the first message is a stable thread id
    /// that no client has to send.
    pub thread_fingerprint: Option<String>,
    pub status: u16,
    pub streamed: bool,
    pub duration_ms: u64,
    pub bytes_up: usize,
    pub bytes_down: usize,
    /// What the provider claims it billed, when it says so.
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub error: Option<String>,
}

impl Audit {
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        // WAL so a dashboard reading the log never blocks a request writing it.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let this = Self {
            conn: Mutex::new(conn),
        };
        this.migrate()?;
        Ok(this)
    }

    #[cfg(test)]
    pub fn in_memory() -> rusqlite::Result<Self> {
        let this = Self {
            conn: Mutex::new(Connection::open_in_memory()?),
        };
        this.migrate()?;
        Ok(this)
    }

    /// A poisoned mutex means a previous holder panicked mid-write. The log is
    /// append-only and every statement is atomic, so the data is still sound and
    /// refusing to log from then on would be worse than carrying on.
    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn migrate(&self) -> rusqlite::Result<()> {
        self.lock().execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS calls (
                id                 INTEGER PRIMARY KEY,
                at                 TEXT    NOT NULL,
                client             TEXT    NOT NULL,
                route              TEXT    NOT NULL,
                provider           TEXT    NOT NULL,
                model_requested    TEXT    NOT NULL,
                model_sent         TEXT    NOT NULL,
                trust              TEXT    NOT NULL,
                agent_fingerprint  TEXT,
                thread_fingerprint TEXT,
                status             INTEGER NOT NULL,
                streamed           INTEGER NOT NULL,
                duration_ms        INTEGER NOT NULL,
                bytes_up           INTEGER NOT NULL,
                bytes_down         INTEGER NOT NULL,
                input_tokens       INTEGER,
                output_tokens      INTEGER,
                cache_read_tokens  INTEGER,
                error              TEXT
            );
            CREATE INDEX IF NOT EXISTS calls_at      ON calls(at);
            CREATE INDEX IF NOT EXISTS calls_client  ON calls(client, at);
            CREATE INDEX IF NOT EXISTS calls_thread  ON calls(thread_fingerprint);
            "#,
        )
    }

    pub fn record(&self, r: &Record) -> rusqlite::Result<()> {
        let at = OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        self.lock().execute(
            r#"INSERT INTO calls (at, client, route, provider, model_requested, model_sent,
                                  trust, agent_fingerprint, thread_fingerprint, status, streamed,
                                  duration_ms, bytes_up, bytes_down, input_tokens, output_tokens,
                                  cache_read_tokens, error)
               VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)"#,
            rusqlite::params![
                at,
                r.client,
                r.route,
                r.provider,
                r.model_requested,
                r.model_sent,
                r.trust,
                r.agent_fingerprint,
                r.thread_fingerprint,
                r.status,
                r.streamed as i64,
                r.duration_ms as i64,
                r.bytes_up as i64,
                r.bytes_down as i64,
                r.input_tokens.map(|v| v as i64),
                r.output_tokens.map(|v| v as i64),
                r.cache_read_tokens.map(|v| v as i64),
                r.error,
            ],
        )?;
        Ok(())
    }

    /// Rows for the dashboard, newest first.
    #[allow(dead_code, reason = "consumed by the dashboard, which lands in V3")]
    pub fn recent(&self, limit: u32) -> rusqlite::Result<Vec<CallSummary>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT at, client, model_sent, status, duration_ms FROM calls ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map([limit], |row| {
                Ok(CallSummary {
                    at: row.get(0)?,
                    client: row.get(1)?,
                    model_sent: row.get(2)?,
                    status: row.get::<_, i64>(3)? as u16,
                    duration_ms: row.get::<_, i64>(4)? as u64,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

/// One line of the dashboard's call list.
#[derive(Debug, Clone)]
#[allow(dead_code, reason = "read by the dashboard, which lands in V3")]
pub struct CallSummary {
    pub at: String,
    pub client: String,
    pub model_sent: String,
    pub status: u16,
    pub duration_ms: u64,
}

/// Short, stable, one-way. Used for both fingerprints, on content that never
/// leaves this process in any other form.
pub fn fingerprint(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    hex::encode(&digest[..8])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_record() -> Record {
        Record {
            client: "workstation".into(),
            route: "haiku".into(),
            provider: "deepseek".into(),
            model_requested: "haiku".into(),
            model_sent: "deepseek-chat".into(),
            trust: "low",
            agent_fingerprint: Some("abc123".into()),
            thread_fingerprint: Some("def456".into()),
            status: 200,
            streamed: true,
            duration_ms: 1234,
            bytes_up: 900,
            bytes_down: 5000,
            input_tokens: Some(700),
            output_tokens: Some(120),
            cache_read_tokens: None,
            error: None,
        }
    }

    #[test]
    fn records_survive_a_round_trip() {
        let a = Audit::in_memory().unwrap();
        a.record(&a_record()).unwrap();
        let rows = a.recent(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].client, "workstation");
        assert_eq!(rows[0].model_sent, "deepseek-chat");
        assert_eq!(rows[0].status, 200);
    }

    #[test]
    fn migrate_is_idempotent() {
        let a = Audit::in_memory().unwrap();
        a.migrate().unwrap();
        a.migrate().unwrap();
        a.record(&a_record()).unwrap();
        assert_eq!(a.recent(10).unwrap().len(), 1);
    }

    #[test]
    fn fingerprints_are_short_stable_and_opaque() {
        let a = fingerprint(b"You are an investigator agent.");
        let b = fingerprint(b"You are an investigator agent.");
        let c = fingerprint(b"You are a builder agent.");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 16);
        assert!(!a.contains("investigator"));
    }
}
