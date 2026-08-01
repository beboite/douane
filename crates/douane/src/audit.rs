//! The request log, and everything the dashboard reads.
//!
//! One row per upstream call. This is what makes attribution possible, and more
//! importantly it is what lets a provider's invoice be checked against what
//! douane actually sent. A reseller that bills for tokens nobody asked for shows
//! up here as a gap, and there is no other place that gap is visible.
//!
//! **No request or response body is stored.** Bodies are where the secrets are,
//! and a log that holds them is a second copy of everything the vault exists to
//! protect. The same goes for scrub alerts: an alert names the vault reference
//! or the pattern that matched, never the text that matched it.
//!
//! A call is written in two steps. It is inserted the moment the routing
//! decision is made, and updated when the response finishes, which for a stream
//! can be a minute later. A row with `status` still null is a call in flight,
//! and one left that way is a call whose stream died, which is worth seeing.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;
use serde::Serialize;
use time::OffsetDateTime;

use crate::config::RouteOverride;
use crate::usage::Usage;

/// A SQLite connection is not `Sync`, and the broker's state is shared across
/// every request task, so the connection lives behind a mutex. With WAL enabled
/// an insert is a sub-millisecond operation, so the contention this introduces
/// is far cheaper than a pool would be to carry.
pub struct Audit {
    conn: Mutex<Connection>,
}

/// What is known when the routing decision is made.
#[derive(Debug, Clone)]
pub struct Start {
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
    pub streamed: bool,
    pub bytes_up: usize,
}

/// One thing the scrubber found on its way out.
///
/// `label` is a vault reference or a pattern name. There is no field here for
/// the matched text, which is the point: an alert table full of leaked
/// credentials would be a worse leak than the one it is reporting.
#[derive(Debug, Clone)]
pub struct NewAlert<'a> {
    pub call_id: Option<i64>,
    pub client: &'a str,
    pub provider: &'a str,
    pub trust: &'a str,
    pub kind: &'a str,
    pub label: &'a str,
    pub action: &'a str,
}

/// What is known once the response is done.
#[derive(Debug, Clone, Default)]
pub struct Finish {
    pub status: u16,
    pub duration_ms: u64,
    pub bytes_down: usize,
    pub usage: Usage,
    /// Priced with the rates in force at the time, and stored, so a later
    /// pricing edit does not silently rewrite history.
    pub cost: Option<f64>,
    pub error: Option<String>,
}

impl Audit {
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        // WAL so the dashboard reading the log never blocks a request writing it.
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
                streamed           INTEGER NOT NULL,
                bytes_up           INTEGER NOT NULL,
                status             INTEGER,
                duration_ms        INTEGER,
                bytes_down         INTEGER,
                input_tokens       INTEGER,
                output_tokens      INTEGER,
                cache_read_tokens  INTEGER,
                cache_write_tokens INTEGER,
                cost               REAL,
                error              TEXT
            );
            CREATE INDEX IF NOT EXISTS calls_at      ON calls(at);
            CREATE INDEX IF NOT EXISTS calls_client  ON calls(client, at);
            CREATE INDEX IF NOT EXISTS calls_thread  ON calls(thread_fingerprint);
            CREATE INDEX IF NOT EXISTS calls_agent   ON calls(agent_fingerprint);

            -- What the scrubber found. `label` is a vault reference or a pattern
            -- name; the matched text is never written here or anywhere else.
            CREATE TABLE IF NOT EXISTS alerts (
                id       INTEGER PRIMARY KEY,
                at       TEXT    NOT NULL,
                call_id  INTEGER,
                client   TEXT    NOT NULL,
                provider TEXT    NOT NULL,
                trust    TEXT    NOT NULL,
                kind     TEXT    NOT NULL,
                label    TEXT    NOT NULL,
                action   TEXT    NOT NULL
            );
            CREATE INDEX IF NOT EXISTS alerts_at ON alerts(at);

            -- The dashboard's live edits to the routing table. The TOML stays
            -- the base and keeps its comments; this holds the diff.
            CREATE TABLE IF NOT EXISTS route_overrides (
                route    TEXT PRIMARY KEY,
                provider TEXT,
                model    TEXT,
                at       TEXT NOT NULL
            );
            "#,
        )
    }

    pub fn begin(&self, s: &Start) -> rusqlite::Result<i64> {
        let conn = self.lock();
        conn.execute(
            r#"INSERT INTO calls (at, client, route, provider, model_requested, model_sent,
                                  trust, agent_fingerprint, thread_fingerprint, streamed, bytes_up)
               VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)"#,
            rusqlite::params![
                now(),
                s.client,
                s.route,
                s.provider,
                s.model_requested,
                s.model_sent,
                s.trust,
                s.agent_fingerprint,
                s.thread_fingerprint,
                s.streamed as i64,
                s.bytes_up as i64,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn finish(&self, id: i64, f: &Finish) -> rusqlite::Result<()> {
        self.lock().execute(
            r#"UPDATE calls SET status=?2, duration_ms=?3, bytes_down=?4,
                                input_tokens=?5, output_tokens=?6,
                                cache_read_tokens=?7, cache_write_tokens=?8,
                                cost=?9, error=?10
               WHERE id=?1"#,
            rusqlite::params![
                id,
                f.status as i64,
                f.duration_ms as i64,
                f.bytes_down as i64,
                f.usage.input_tokens.map(|v| v as i64),
                f.usage.output_tokens.map(|v| v as i64),
                f.usage.cache_read_tokens.map(|v| v as i64),
                f.usage.cache_write_tokens.map(|v| v as i64),
                f.cost,
                f.error,
            ],
        )?;
        Ok(())
    }

    pub fn alert(&self, a: &NewAlert<'_>) -> rusqlite::Result<()> {
        self.lock().execute(
            r#"INSERT INTO alerts (at, call_id, client, provider, trust, kind, label, action)
               VALUES (?1,?2,?3,?4,?5,?6,?7,?8)"#,
            rusqlite::params![
                now(),
                a.call_id,
                a.client,
                a.provider,
                a.trust,
                a.kind,
                a.label,
                a.action
            ],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------- overrides --

    pub fn overrides(&self) -> rusqlite::Result<BTreeMap<String, RouteOverride>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT route, provider, model FROM route_overrides")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                RouteOverride {
                    provider: r.get(1)?,
                    model: r.get(2)?,
                },
            ))
        })?;
        rows.collect()
    }

    pub fn set_override(&self, route: &str, o: &RouteOverride) -> rusqlite::Result<()> {
        self.lock().execute(
            r#"INSERT INTO route_overrides (route, provider, model, at) VALUES (?1,?2,?3,?4)
               ON CONFLICT(route) DO UPDATE SET provider=?2, model=?3, at=?4"#,
            rusqlite::params![route, o.provider, o.model, now()],
        )?;
        Ok(())
    }

    pub fn clear_override(&self, route: &str) -> rusqlite::Result<bool> {
        let n = self
            .lock()
            .execute("DELETE FROM route_overrides WHERE route=?1", [route])?;
        Ok(n > 0)
    }

    // ------------------------------------------------------------ dashboard --

    /// Newest calls first, optionally narrowed to one client or one thread.
    pub fn recent(
        &self,
        limit: u32,
        client: Option<&str>,
        thread: Option<&str>,
    ) -> rusqlite::Result<Vec<Call>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            r#"SELECT id, at, client, route, provider, model_requested, model_sent, trust,
                      agent_fingerprint, thread_fingerprint, streamed, status, duration_ms,
                      bytes_up, bytes_down, input_tokens, output_tokens, cache_read_tokens,
                      cache_write_tokens, cost, error
               FROM calls
               WHERE (?2 IS NULL OR client = ?2) AND (?3 IS NULL OR thread_fingerprint = ?3)
               ORDER BY id DESC LIMIT ?1"#,
        )?;
        let rows = stmt.query_map(rusqlite::params![limit, client, thread], |r| {
            Ok(Call {
                id: r.get(0)?,
                at: r.get(1)?,
                client: r.get(2)?,
                route: r.get(3)?,
                provider: r.get(4)?,
                model_requested: r.get(5)?,
                model_sent: r.get(6)?,
                trust: r.get(7)?,
                agent_fingerprint: r.get(8)?,
                thread_fingerprint: r.get(9)?,
                streamed: r.get::<_, i64>(10)? != 0,
                status: r.get::<_, Option<i64>>(11)?.map(|v| v as u16),
                duration_ms: r.get(12)?,
                bytes_up: r.get(13)?,
                bytes_down: r.get(14)?,
                input_tokens: r.get(15)?,
                output_tokens: r.get(16)?,
                cache_read_tokens: r.get(17)?,
                cache_write_tokens: r.get(18)?,
                cost: r.get(19)?,
                error: r.get(20)?,
            })
        })?;
        rows.collect()
    }

    /// Totals plus the four breakdowns the dashboard shows side by side.
    ///
    /// `since` is an RFC 3339 timestamp compared as a string, which works
    /// because every `at` is written in the same fixed-width UTC format.
    pub fn summary(&self, since: &str) -> rusqlite::Result<Summary> {
        Ok(Summary {
            totals: self.totals(since)?,
            by_provider: self.grouped(since, "provider")?,
            by_client: self.grouped(since, "client")?,
            by_route: self.grouped(since, "route")?,
            by_agent: self.grouped(since, "COALESCE(agent_fingerprint, 'none')")?,
        })
    }

    fn totals(&self, since: &str) -> rusqlite::Result<Bucket> {
        let conn = self.lock();
        conn.query_row(
            r#"SELECT 'all', COUNT(*),
                      SUM(CASE WHEN status >= 400 OR error IS NOT NULL THEN 1 ELSE 0 END),
                      COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                      COALESCE(SUM(cache_read_tokens),0), COALESCE(SUM(cost),0)
               FROM calls WHERE at >= ?1"#,
            [since],
            bucket_from_row,
        )
    }

    /// `by` is a column name or a SQL expression built from one, never anything
    /// a request can influence: every call site below passes a literal.
    fn grouped(&self, since: &str, by: &str) -> rusqlite::Result<Vec<Bucket>> {
        let conn = self.lock();
        let sql = format!(
            r#"SELECT {by} AS k, COUNT(*),
                      SUM(CASE WHEN status >= 400 OR error IS NOT NULL THEN 1 ELSE 0 END),
                      COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                      COALESCE(SUM(cache_read_tokens),0), COALESCE(SUM(cost),0)
               FROM calls WHERE at >= ?1 GROUP BY k ORDER BY COUNT(*) DESC"#
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([since], bucket_from_row)?;
        rows.collect()
    }

    /// Conversations, newest first. One row per thread fingerprint.
    pub fn threads(&self, since: &str, limit: u32) -> rusqlite::Result<Vec<Thread>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            r#"SELECT thread_fingerprint, MIN(at), MAX(at), COUNT(*),
                      GROUP_CONCAT(DISTINCT client), GROUP_CONCAT(DISTINCT provider),
                      COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                      COALESCE(SUM(cost),0)
               FROM calls
               WHERE at >= ?1 AND thread_fingerprint IS NOT NULL
               GROUP BY thread_fingerprint ORDER BY MAX(at) DESC LIMIT ?2"#,
        )?;
        let rows = stmt.query_map(rusqlite::params![since, limit], |r| {
            Ok(Thread {
                fingerprint: r.get(0)?,
                first_at: r.get(1)?,
                last_at: r.get(2)?,
                calls: r.get(3)?,
                clients: r.get(4)?,
                providers: r.get(5)?,
                input_tokens: r.get(6)?,
                output_tokens: r.get(7)?,
                cost: r.get(8)?,
            })
        })?;
        rows.collect()
    }

    pub fn alerts(&self, limit: u32) -> rusqlite::Result<Vec<Alert>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            r#"SELECT id, at, call_id, client, provider, trust, kind, label, action
               FROM alerts ORDER BY id DESC LIMIT ?1"#,
        )?;
        let rows = stmt.query_map([limit], |r| {
            Ok(Alert {
                id: r.get(0)?,
                at: r.get(1)?,
                call_id: r.get(2)?,
                client: r.get(3)?,
                provider: r.get(4)?,
                trust: r.get(5)?,
                kind: r.get(6)?,
                label: r.get(7)?,
                action: r.get(8)?,
            })
        })?;
        rows.collect()
    }
}

fn bucket_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Bucket> {
    Ok(Bucket {
        key: r.get(0)?,
        calls: r.get(1)?,
        errors: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
        input_tokens: r.get(3)?,
        output_tokens: r.get(4)?,
        cache_read_tokens: r.get(5)?,
        cost: r.get(6)?,
    })
}

fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

#[derive(Debug, Clone, Serialize)]
pub struct Call {
    pub id: i64,
    pub at: String,
    pub client: String,
    pub route: String,
    pub provider: String,
    pub model_requested: String,
    pub model_sent: String,
    pub trust: String,
    pub agent_fingerprint: Option<String>,
    pub thread_fingerprint: Option<String>,
    pub streamed: bool,
    pub status: Option<u16>,
    pub duration_ms: Option<i64>,
    pub bytes_up: i64,
    pub bytes_down: Option<i64>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub cost: Option<f64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Bucket {
    pub key: String,
    pub calls: i64,
    pub errors: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub totals: Bucket,
    pub by_provider: Vec<Bucket>,
    pub by_client: Vec<Bucket>,
    pub by_route: Vec<Bucket>,
    pub by_agent: Vec<Bucket>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Thread {
    pub fingerprint: String,
    pub first_at: String,
    pub last_at: String,
    pub calls: i64,
    pub clients: String,
    pub providers: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Alert {
    pub id: i64,
    pub at: String,
    pub call_id: Option<i64>,
    pub client: String,
    pub provider: String,
    pub trust: String,
    pub kind: String,
    pub label: String,
    pub action: String,
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

    fn a_start() -> Start {
        Start {
            client: "workstation".into(),
            route: "haiku".into(),
            provider: "deepseek".into(),
            model_requested: "haiku".into(),
            model_sent: "deepseek-chat".into(),
            trust: "low",
            agent_fingerprint: Some("abc123".into()),
            thread_fingerprint: Some("def456".into()),
            streamed: true,
            bytes_up: 900,
        }
    }

    fn a_finish() -> Finish {
        Finish {
            status: 200,
            duration_ms: 1234,
            bytes_down: 5000,
            usage: Usage {
                input_tokens: Some(700),
                output_tokens: Some(120),
                cache_read_tokens: Some(50),
                cache_write_tokens: None,
            },
            cost: Some(0.0042),
            error: None,
        }
    }

    #[test]
    fn a_call_survives_both_halves_of_its_write() {
        let a = Audit::in_memory().unwrap();
        let id = a.begin(&a_start()).unwrap();

        // Before the response comes back the row exists with no status: that is
        // what "in flight" looks like, and a row stuck here is a dead stream.
        let mid = a.recent(10, None, None).unwrap();
        assert_eq!(mid.len(), 1);
        assert_eq!(mid[0].status, None);
        assert_eq!(mid[0].input_tokens, None);

        a.finish(id, &a_finish()).unwrap();
        let done = a.recent(10, None, None).unwrap();
        assert_eq!(done[0].status, Some(200));
        assert_eq!(done[0].input_tokens, Some(700));
        assert_eq!(done[0].bytes_down, Some(5000));
        assert_eq!(done[0].cost, Some(0.0042));
    }

    #[test]
    fn filters_narrow_to_one_client_and_one_thread() {
        let a = Audit::in_memory().unwrap();
        a.begin(&a_start()).unwrap();
        let mut other = a_start();
        other.client = "laptop".into();
        other.thread_fingerprint = Some("zzz".into());
        a.begin(&other).unwrap();

        assert_eq!(a.recent(10, None, None).unwrap().len(), 2);
        assert_eq!(a.recent(10, Some("laptop"), None).unwrap().len(), 1);
        assert_eq!(a.recent(10, None, Some("def456")).unwrap().len(), 1);
        assert_eq!(
            a.recent(10, Some("laptop"), Some("def456")).unwrap().len(),
            0
        );
    }

    #[test]
    fn the_summary_attributes_along_all_four_axes() {
        let a = Audit::in_memory().unwrap();
        for _ in 0..3 {
            let id = a.begin(&a_start()).unwrap();
            a.finish(id, &a_finish()).unwrap();
        }
        let mut other = a_start();
        other.provider = "anthropic".into();
        other.agent_fingerprint = Some("builder".into());
        let id = a.begin(&other).unwrap();
        a.finish(
            id,
            &Finish {
                status: 500,
                error: Some("upstream exploded".into()),
                ..a_finish()
            },
        )
        .unwrap();

        let s = a.summary("1970-01-01T00:00:00Z").unwrap();
        assert_eq!(s.totals.calls, 4);
        assert_eq!(s.totals.errors, 1);
        assert_eq!(s.totals.input_tokens, 2800);
        assert_eq!(s.by_provider.len(), 2);
        assert_eq!(s.by_provider[0].key, "deepseek");
        assert_eq!(s.by_provider[0].calls, 3);
        assert_eq!(s.by_agent.len(), 2);
        assert!((s.totals.cost - 0.0168).abs() < 1e-9);
    }

    #[test]
    fn threads_group_the_turns_of_one_conversation() {
        let a = Audit::in_memory().unwrap();
        for _ in 0..3 {
            let id = a.begin(&a_start()).unwrap();
            a.finish(id, &a_finish()).unwrap();
        }
        let t = a.threads("1970-01-01T00:00:00Z", 10).unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].fingerprint, "def456");
        assert_eq!(t[0].calls, 3);
        assert_eq!(t[0].output_tokens, 360);
    }

    #[test]
    fn an_alert_names_the_reference_and_not_the_value() {
        let a = Audit::in_memory().unwrap();
        a.alert(&NewAlert {
            call_id: Some(1),
            client: "workstation",
            provider: "deepseek",
            trust: "low",
            kind: "vault",
            label: "proxy.password",
            action: "block",
        })
        .unwrap();
        let rows = a.alerts(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "proxy.password");
        assert_eq!(rows[0].action, "block");
        let json = serde_json::to_string(&rows).unwrap();
        assert!(json.contains("proxy.password"));
    }

    #[test]
    fn overrides_round_trip_and_clear() {
        let a = Audit::in_memory().unwrap();
        assert!(a.overrides().unwrap().is_empty());

        a.set_override(
            "haiku",
            &RouteOverride {
                provider: Some("anthropic".into()),
                model: Some("claude-haiku-4-5".into()),
            },
        )
        .unwrap();
        // Setting twice is an update, not a duplicate row.
        a.set_override(
            "haiku",
            &RouteOverride {
                provider: Some("local".into()),
                model: None,
            },
        )
        .unwrap();

        let o = a.overrides().unwrap();
        assert_eq!(o.len(), 1);
        assert_eq!(o["haiku"].provider.as_deref(), Some("local"));
        assert_eq!(o["haiku"].model, None);

        assert!(a.clear_override("haiku").unwrap());
        assert!(!a.clear_override("haiku").unwrap());
        assert!(a.overrides().unwrap().is_empty());
    }

    #[test]
    fn migrate_is_idempotent() {
        let a = Audit::in_memory().unwrap();
        a.migrate().unwrap();
        a.migrate().unwrap();
        a.begin(&a_start()).unwrap();
        assert_eq!(a.recent(10, None, None).unwrap().len(), 1);
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
