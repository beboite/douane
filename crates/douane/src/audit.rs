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
    /// The unit `cost` is in, stored per call for the same reason. Two upstreams
    /// bill in two currencies, so a bare number is not a cost.
    pub currency: Option<String>,
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
                currency           TEXT,
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
        )?;

        // `calls` predates per-currency pricing. `CREATE TABLE IF NOT EXISTS`
        // leaves an existing table exactly as it was, so a deployed database
        // needs the column added rather than declared. Rows written before this
        // keep a NULL currency and are reported as unpriced, which is what they
        // were.
        self.add_column_if_missing("calls", "currency", "TEXT")
    }

    /// `ALTER TABLE ADD COLUMN` is an error when the column is already there,
    /// and SQLite has no `IF NOT EXISTS` for it. Asking first is the only way
    /// that does not involve matching on an error string.
    fn add_column_if_missing(&self, table: &str, column: &str, ty: &str) -> rusqlite::Result<()> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let mut present = stmt.query_map([], |r| r.get::<_, String>(1))?;
        if present.any(|c| c.as_deref() == Ok(column)) {
            return Ok(());
        }
        drop(present);
        drop(stmt);
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {ty}"))
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
                                cost=?9, currency=?10, error=?11
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
                // Only stored alongside a cost. A currency on a row with no
                // price would claim the call was free in that currency.
                f.cost.and(f.currency.as_deref()),
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
                      cache_write_tokens, cost, currency, error
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
                currency: r.get(20)?,
                error: r.get(21)?,
            })
        })?;
        rows.collect()
    }

    /// Totals plus the four breakdowns the dashboard shows side by side, and the
    /// same window cut into slots so it can be drawn rather than only read.
    ///
    /// `since` is an RFC 3339 timestamp compared as a string, which works
    /// because every `at` is written in the same fixed-width UTC format.
    pub fn summary(&self, since: &str, hours: i64) -> rusqlite::Result<Summary> {
        Ok(Summary {
            totals: self.totals(since)?,
            by_provider: self.grouped(since, "provider")?,
            by_client: self.grouped(since, "client")?,
            by_route: self.grouped(since, "route")?,
            by_agent: self.grouped(since, "COALESCE(agent_fingerprint, 'none')")?,
            series: self.series(since, hours)?,
        })
    }

    /// The window cut into `SLOTS` equal slices, empty ones included.
    ///
    /// The empties are the point. A curve drawn from the rows alone puts two
    /// calls an hour apart next to each other and reads as steady traffic, so a
    /// quiet night would look identical to a busy one. Filling the gaps here
    /// means the shape of the chart is the shape of the day.
    ///
    /// Slot width comes from the window, so the 24 h view and the 30 d view both
    /// return the same number of points and the front end never has to know how
    /// wide one is: the timestamp on each point says it.
    fn series(&self, since: &str, hours: i64) -> rusqlite::Result<Vec<Slot>> {
        const SLOTS: i64 = 60;

        let start = OffsetDateTime::parse(since, &time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| OffsetDateTime::now_utc());
        let origin = start.unix_timestamp();
        // A minute is the floor: below it the slots outnumber the seconds in the
        // window and every point but one is empty by construction.
        let width = ((hours * 3600) / SLOTS).max(60);

        let conn = self.lock();
        // `unixepoch` parses the stored RFC 3339 directly, so the bucketing is
        // integer division on the server and no row crosses the wire to be
        // bucketed here.
        let mut stmt = conn.prepare(
            r#"SELECT CAST((unixepoch(at) - ?2) / ?3 AS INTEGER) AS slot, currency, COUNT(*),
                      SUM(CASE WHEN status >= 400 OR error IS NOT NULL THEN 1 ELSE 0 END),
                      COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                      COALESCE(SUM(cost),0)
               FROM calls WHERE at >= ?1 GROUP BY slot, currency"#,
        )?;
        let rows = stmt.query_map(rusqlite::params![since, origin, width], |r| {
            Ok((
                // Null when `unixepoch` could not read the timestamp back. One
                // malformed row must not take the whole dashboard down with a
                // 500, so it loses its place on the curve and nothing else.
                r.get::<_, Option<i64>>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, f64>(6)?,
            ))
        })?;

        let mut out: Vec<Slot> = (0..SLOTS)
            .map(|i| Slot {
                at: (start + time::Duration::seconds(i * width))
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_default(),
                seconds: width,
                ..Default::default()
            })
            .collect();

        for row in rows {
            let (slot, currency, calls, errors, input, output, cost) = row?;
            let Some(slot) = slot else { continue };
            // A call logged between `since` being computed and the query running
            // lands one past the end. Clamping keeps it in the last slot rather
            // than dropping it, which would make the newest point read as a lull.
            let i = slot.clamp(0, SLOTS - 1) as usize;
            let s = &mut out[i];
            s.calls += calls;
            s.errors += errors;
            s.input_tokens += input;
            s.output_tokens += output;
            if let Some(c) = currency {
                *s.cost.entry(c).or_default() += cost;
            }
        }
        Ok(out)
    }

    fn totals(&self, since: &str) -> rusqlite::Result<Bucket> {
        let mut all = self.grouped(since, "'all'")?;
        // One group, or none at all when the window is empty.
        Ok(all.pop().unwrap_or_else(|| Bucket {
            key: "all".to_owned(),
            ..Default::default()
        }))
    }

    /// `by` is a column name or a SQL expression built from one, never anything
    /// a request can influence: every call site below passes a literal.
    ///
    /// Grouped by key *and* currency, then folded back in Rust. SQL can sum a
    /// column but it cannot keep two units apart in one number, and a bucket
    /// whose calls went to a reseller billing CNY and one billing USD has two
    /// costs, not one.
    fn grouped(&self, since: &str, by: &str) -> rusqlite::Result<Vec<Bucket>> {
        let conn = self.lock();
        let sql = format!(
            r#"SELECT {by} AS k, currency, COUNT(*),
                      SUM(CASE WHEN status >= 400 OR error IS NOT NULL THEN 1 ELSE 0 END),
                      COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                      COALESCE(SUM(cache_read_tokens),0), COALESCE(SUM(cost),0)
               FROM calls WHERE at >= ?1 GROUP BY k, currency"#
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([since], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, f64>(7)?,
            ))
        })?;

        let mut out: BTreeMap<String, Bucket> = BTreeMap::new();
        for row in rows {
            let (key, currency, calls, errors, input, output, cache_read, cost) = row?;
            let b = out.entry(key.clone()).or_insert_with(|| Bucket {
                key,
                ..Default::default()
            });
            b.calls += calls;
            b.errors += errors;
            b.input_tokens += input;
            b.output_tokens += output;
            b.cache_read_tokens += cache_read;
            if let Some(c) = currency {
                *b.cost.entry(c).or_default() += cost;
            }
        }

        let mut buckets: Vec<Bucket> = out.into_values().collect();
        buckets.sort_by_key(|b| std::cmp::Reverse(b.calls));
        Ok(buckets)
    }

    /// Conversations, newest first. One row per thread fingerprint.
    pub fn threads(&self, since: &str, limit: u32) -> rusqlite::Result<Vec<Thread>> {
        let conn = self.lock();
        // The cost comes from a second pass rather than a join: a thread that
        // was rerouted mid-conversation spans two providers and therefore
        // possibly two currencies, and there is no single SUM for that.
        let mut stmt = conn.prepare(
            r#"SELECT thread_fingerprint, MIN(at), MAX(at), COUNT(*),
                      GROUP_CONCAT(DISTINCT client), GROUP_CONCAT(DISTINCT provider),
                      COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0)
               FROM calls
               WHERE at >= ?1 AND thread_fingerprint IS NOT NULL
               GROUP BY thread_fingerprint ORDER BY MAX(at) DESC LIMIT ?2"#,
        )?;
        let mut threads: Vec<Thread> = stmt
            .query_map(rusqlite::params![since, limit], |r| {
                Ok(Thread {
                    fingerprint: r.get(0)?,
                    first_at: r.get(1)?,
                    last_at: r.get(2)?,
                    calls: r.get(3)?,
                    clients: r.get(4)?,
                    providers: r.get(5)?,
                    input_tokens: r.get(6)?,
                    output_tokens: r.get(7)?,
                    cost: Money::new(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;

        let mut costs = conn.prepare(
            r#"SELECT thread_fingerprint, currency, COALESCE(SUM(cost),0)
               FROM calls
               WHERE at >= ?1 AND thread_fingerprint IS NOT NULL AND currency IS NOT NULL
               GROUP BY thread_fingerprint, currency"#,
        )?;
        let mut by_thread: BTreeMap<String, Money> = BTreeMap::new();
        for row in costs.query_map([since], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
            ))
        })? {
            let (fingerprint, currency, cost) = row?;
            *by_thread
                .entry(fingerprint)
                .or_default()
                .entry(currency)
                .or_default() += cost;
        }
        for t in &mut threads {
            if let Some(m) = by_thread.remove(&t.fingerprint) {
                t.cost = m;
            }
        }
        Ok(threads)
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
    pub currency: Option<String>,
    pub error: Option<String>,
}

/// Costs held apart by unit.
///
/// Adding a price quoted in CNY to one quoted in USD produces a number that is
/// not money in any currency. Since douane deliberately never converts, every
/// total it reports is a small map rather than a scalar, and a reader sees
/// `0.42 USD + 3.10 CNY` instead of a confident `3.52`. Empty means nothing in
/// the window was priced.
pub type Money = BTreeMap<String, f64>;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Bucket {
    pub key: String,
    pub calls: i64,
    pub errors: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cost: Money,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub totals: Bucket,
    pub by_provider: Vec<Bucket>,
    pub by_client: Vec<Bucket>,
    pub by_route: Vec<Bucket>,
    pub by_agent: Vec<Bucket>,
    pub series: Vec<Slot>,
}

/// One point on the activity curve.
///
/// `seconds` travels with the point because the slot width depends on the
/// window that was asked for, and a chart that has to guess it labels the axis
/// wrong on every window but the one it was written against.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Slot {
    pub at: String,
    pub seconds: i64,
    pub calls: i64,
    pub errors: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost: Money,
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
    pub cost: Money,
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
            currency: Some("USD".into()),
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

        let s = a.summary("1970-01-01T00:00:00Z", 24).unwrap();
        assert_eq!(s.totals.calls, 4);
        assert_eq!(s.totals.errors, 1);
        assert_eq!(s.totals.input_tokens, 2800);
        assert_eq!(s.by_provider.len(), 2);
        assert_eq!(s.by_provider[0].key, "deepseek");
        assert_eq!(s.by_provider[0].calls, 3);
        assert_eq!(s.by_agent.len(), 2);
        assert!((s.totals.cost["USD"] - 0.0168).abs() < 1e-9);
    }

    #[test]
    fn the_curve_keeps_its_empty_slots() {
        // Two calls an hour apart over a day. Drawn from the rows alone they
        // would sit side by side and read as continuous traffic, which is the
        // whole reason the empties are carried.
        let a = Audit::in_memory().unwrap();
        let start = OffsetDateTime::now_utc() - time::Duration::hours(24);
        let fmt = &time::format_description::well_known::Rfc3339;
        for offset in [2i64, 3] {
            let id = a.begin(&a_start()).unwrap();
            a.finish(id, &a_finish()).unwrap();
            let at = (start + time::Duration::hours(offset)).format(fmt).unwrap();
            a.lock()
                .execute("UPDATE calls SET at = ?1 WHERE id = ?2", (at, id))
                .unwrap();
        }

        let s = a.summary(&start.format(fmt).unwrap(), 24).unwrap();
        assert_eq!(s.series.len(), 60);
        assert_eq!(s.totals.calls, 2);
        // 24 h over 60 slots is 24 minutes each, so two calls an hour apart land
        // in slots of their own with quiet ones between them.
        let busy: Vec<usize> = s
            .series
            .iter()
            .enumerate()
            .filter(|(_, p)| p.calls > 0)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(busy.len(), 2, "two calls, two slots");
        assert!(busy[1] - busy[0] > 1, "with silence between them");
        assert_eq!(s.series.iter().map(|p| p.calls).sum::<i64>(), 2);
        assert!(s.series.iter().all(|p| p.seconds == 24 * 60));
    }

    #[test]
    fn a_timestamp_nothing_can_read_costs_its_own_point_and_nothing_else() {
        // A row whose `at` cannot be parsed used to answer the whole summary
        // with a 500, so one bad write hid every good one behind it.
        let a = Audit::in_memory().unwrap();
        for _ in 0..2 {
            let id = a.begin(&a_start()).unwrap();
            a.finish(id, &a_finish()).unwrap();
        }
        a.lock()
            .execute("UPDATE calls SET at = 'pas une date' WHERE id = 1", ())
            .unwrap();

        let s = a.summary("1970-01-01T00:00:00Z", 24).unwrap();
        assert_eq!(s.totals.calls, 2, "the totals still count it");
        assert_eq!(
            s.series.iter().map(|p| p.calls).sum::<i64>(),
            1,
            "the curve cannot place it, and drops that one alone"
        );
    }

    #[test]
    fn a_window_short_enough_never_slices_below_a_minute() {
        // 60 slots over one hour would be a slot a minute, which is the floor.
        // Anything shorter has to widen instead, or every point but one is empty
        // by construction and the chart says nothing.
        let a = Audit::in_memory().unwrap();
        let id = a.begin(&a_start()).unwrap();
        a.finish(id, &a_finish()).unwrap();
        let since = (OffsetDateTime::now_utc() - time::Duration::hours(1))
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();

        let s = a.summary(&since, 1).unwrap();
        assert!(s.series.iter().all(|p| p.seconds == 60));
        assert_eq!(s.series.iter().map(|p| p.calls).sum::<i64>(), 1);
    }

    #[test]
    fn two_currencies_are_reported_apart_and_never_added() {
        // The case this exists for: one reseller quotes CNY, another quotes
        // USD. A single total would be a number in no currency at all.
        let a = Audit::in_memory().unwrap();
        let id = a.begin(&a_start()).unwrap();
        a.finish(id, &a_finish()).unwrap();

        let mut cny = a_start();
        cny.provider = "example-reseller".into();
        let id = a.begin(&cny).unwrap();
        a.finish(
            id,
            &Finish {
                cost: Some(1.5),
                currency: Some("CNY".into()),
                ..a_finish()
            },
        )
        .unwrap();

        let s = a.summary("1970-01-01T00:00:00Z", 24).unwrap();
        assert_eq!(s.totals.calls, 2, "both calls counted once");
        assert_eq!(s.totals.cost.len(), 2, "two units, two entries");
        assert!((s.totals.cost["USD"] - 0.0042).abs() < 1e-9);
        assert!((s.totals.cost["CNY"] - 1.5).abs() < 1e-9);

        // And each provider keeps only its own unit.
        let xcode = s
            .by_provider
            .iter()
            .find(|b| b.key == "example-reseller")
            .expect("the CNY provider is in the breakdown");
        assert_eq!(xcode.cost.keys().collect::<Vec<_>>(), ["CNY"]);
    }

    #[test]
    fn an_unpriced_call_adds_no_currency_at_all() {
        // A provider with no price list must not turn into a zero: a zero in
        // the dashboard reads as "this one is free".
        let a = Audit::in_memory().unwrap();
        let id = a.begin(&a_start()).unwrap();
        a.finish(
            id,
            &Finish {
                cost: None,
                currency: Some("USD".into()),
                ..a_finish()
            },
        )
        .unwrap();

        let s = a.summary("1970-01-01T00:00:00Z", 24).unwrap();
        assert_eq!(s.totals.calls, 1);
        assert!(s.totals.cost.is_empty(), "no cost, so no currency either");
        assert_eq!(a.recent(1, None, None).unwrap()[0].currency, None);
    }

    #[test]
    fn a_database_written_before_currency_existed_still_opens() {
        // What a deployed instance looks like: the table already exists without
        // the column, and `CREATE TABLE IF NOT EXISTS` will not add it.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"CREATE TABLE calls (
                   id INTEGER PRIMARY KEY, at TEXT NOT NULL, client TEXT NOT NULL,
                   route TEXT NOT NULL, provider TEXT NOT NULL,
                   model_requested TEXT NOT NULL, model_sent TEXT NOT NULL,
                   trust TEXT NOT NULL, agent_fingerprint TEXT, thread_fingerprint TEXT,
                   streamed INTEGER NOT NULL, bytes_up INTEGER NOT NULL, status INTEGER,
                   duration_ms INTEGER, bytes_down INTEGER, input_tokens INTEGER,
                   output_tokens INTEGER, cache_read_tokens INTEGER,
                   cache_write_tokens INTEGER, cost REAL, error TEXT);
               INSERT INTO calls (at, client, route, provider, model_requested, model_sent,
                                  trust, streamed, bytes_up, status, cost)
               VALUES ('2026-01-01T00:00:00Z','old','haiku','deepseek','haiku',
                       'deepseek-chat','low',0,10,200,0.5);"#,
        )
        .unwrap();

        let a = Audit {
            conn: Mutex::new(conn),
        };
        a.migrate().expect("the column is added, not redeclared");
        // Twice, because a restart runs the migration again.
        a.migrate().expect("adding it a second time is a no-op");

        let old = &a.recent(10, None, None).unwrap()[0];
        assert_eq!(old.cost, Some(0.5));
        assert_eq!(old.currency, None, "an old row claims no unit");
        let s = a.summary("1970-01-01T00:00:00Z", 24).unwrap();
        assert_eq!(s.totals.calls, 1);
        assert!(s.totals.cost.is_empty(), "a cost with no unit is not money");
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
