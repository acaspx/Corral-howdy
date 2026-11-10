//! The append-only, hash-chained SQLite event store (ADR-002, system design §6, security model
//! T11).
//!
//! - One `events` table in WAL mode, one row per event. Triggers refuse `UPDATE` and `DELETE`.
//! - Each run is a hash chain: an event's `hash` is SHA-256 over the previous event's hash and the
//!   event's canonical JSON ([`Event::to_json`]). The first event of a run chains from
//!   [`GENESIS`].
//! - [`Store::open`] verifies every chain in one ordered query. If anything was edited, removed or
//!   forged, the store reopens **read-only** and [`Store::integrity`] says where, so the app can
//!   show a banner. History stays readable.
//! - Reads load a run's events in one ordered query (AGENTS.md: no N+1).
//!
//! Not here yet: projections (TP-STORE-02), blob offload over 64 KB (TP-STORE-03) and the
//! Keychain-MACed head hash that catches a rewrite of a whole run or a truncated tail
//! (TP-STORE-06).

use corral_core::event::{Event, EventError};
use corral_core::ids::RunId;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

/// The `prev_hash` of a run's first event.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// How long a writer waits for another writer's transaction before giving up.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS events (
    id             INTEGER PRIMARY KEY,
    run_id         TEXT    NOT NULL,
    seq            INTEGER NOT NULL,
    ts             INTEGER NOT NULL,
    kind           TEXT    NOT NULL,
    span_id        TEXT    NOT NULL,
    parent_span_id TEXT,
    payload_json   TEXT    NOT NULL,
    prev_hash      TEXT    NOT NULL,
    hash           TEXT    NOT NULL,
    UNIQUE (run_id, seq)
);
CREATE TRIGGER IF NOT EXISTS events_no_update BEFORE UPDATE ON events
    BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;
CREATE TRIGGER IF NOT EXISTS events_no_delete BEFORE DELETE ON events
    BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;
";

const COLUMNS: &str =
    "run_id, seq, ts, kind, span_id, parent_span_id, payload_json, prev_hash, hash";

/// What went wrong in the store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// SQLite reported an error.
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// An event couldn't be serialized or read back.
    #[error(transparent)]
    Event(#[from] EventError),
    /// The event's `seq` isn't the next one for its run.
    #[error("run {run_id}: expected seq {expected}, got {got}")]
    OutOfOrder {
        /// The run.
        run_id: RunId,
        /// The next seq the run accepts.
        expected: u64,
        /// The seq that was offered.
        got: u64,
    },
    /// The store failed verification and accepts no writes.
    #[error("the event store is read-only because its history failed verification")]
    ReadOnly,
    /// A stored value is out of range (for example a negative `seq`).
    #[error("corrupt row: {0}")]
    Corrupt(String),
    /// Another thread panicked while holding the store.
    #[error("the store's lock is poisoned")]
    Poisoned,
}

/// Whether the history on disk is the history Corral wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Integrity {
    /// Every run's hash chain checks out.
    Verified,
    /// The first place verification failed.
    Tampered {
        /// The run whose chain broke.
        run_id: RunId,
        /// The event where it broke (for a removed event, the one that's missing).
        seq: u64,
        /// What didn't match.
        reason: String,
    },
}

impl Integrity {
    /// The banner text the app shows when the history was changed, or `None` when it's intact.
    pub fn banner(&self) -> Option<String> {
        match self {
            Integrity::Verified => None,
            Integrity::Tampered { run_id, seq, .. } => Some(format!(
                "Corral's history was changed outside the app (run {run_id}, event {seq}). \
                 History is read-only until you review it."
            )),
        }
    }
}

/// The event store. It's `Send + Sync`, and several stores (or processes) can share one
/// database file.
#[derive(Debug)]
pub struct Store {
    conn: Mutex<Connection>,
    integrity: Integrity,
    path: PathBuf,
}

impl Store {
    /// Opens (or creates) the store at `path` and verifies every run's chain. A store whose
    /// history fails verification still opens, read-only, with [`Store::integrity`] saying where.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        configure(&conn)?;
        conn.execute_batch(SCHEMA)?;

        let integrity = verify(&conn)?;
        let conn = match integrity {
            Integrity::Verified => conn,
            Integrity::Tampered { .. } => {
                drop(conn);
                let ro = Connection::open_with_flags(
                    path,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )?;
                ro.busy_timeout(BUSY_TIMEOUT)?;
                ro
            }
        };
        Ok(Self {
            conn: Mutex::new(conn),
            integrity,
            path: path.to_owned(),
        })
    }

    /// The result of the verification done when the store opened.
    pub fn integrity(&self) -> &Integrity {
        &self.integrity
    }

    /// True when the history failed verification: reads work, appends are refused.
    pub fn is_read_only(&self) -> bool {
        self.integrity != Integrity::Verified
    }

    /// Where the database lives.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends `event` as the next event of its run and returns its hash. `event.seq` must be
    /// exactly one more than the run's last seq (1 for a new run).
    pub fn append(&self, event: &Event) -> Result<String, StoreError> {
        if self.is_read_only() {
            return Err(StoreError::ReadOnly);
        }
        let mut conn = self.lock()?;
        // IMMEDIATE takes the write lock up front, so two writers can't both read the same tail.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let tail: Option<(i64, String)> = tx
            .query_row(
                "SELECT seq, hash FROM events WHERE run_id = ?1 ORDER BY seq DESC LIMIT 1",
                params![event.run_id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (expected, prev_hash) = match tail {
            Some((seq, hash)) => (from_db(seq, "seq")? + 1, hash),
            None => (1, GENESIS.to_owned()),
        };
        if event.seq != expected {
            return Err(StoreError::OutOfOrder {
                run_id: event.run_id.clone(),
                expected,
                got: event.seq,
            });
        }

        let hash = chain_hash(&prev_hash, &event.to_json()?);
        tx.execute(
            &format!("INSERT INTO events ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"),
            params![
                event.run_id.as_str(),
                to_db(event.seq, "seq")?,
                to_db(event.ts, "ts")?,
                event.kind().as_str(),
                event.span_id.as_str(),
                event.parent_span_id.as_ref().map(|s| s.as_str()),
                payload_json(event)?,
                prev_hash,
                hash,
            ],
        )?;
        tx.commit()?;
        Ok(hash)
    }

    /// Every event of `run_id`, in `seq` order, in one query. Empty if the run doesn't exist.
    pub fn read_run(&self, run_id: &RunId) -> Result<Vec<Event>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM events WHERE run_id = ?1 ORDER BY seq"
        ))?;
        let rows = stmt.query_map(params![run_id.as_str()], Row::read)?;
        rows.map(|row| row?.event()).collect()
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, StoreError> {
        self.conn.lock().map_err(|_| StoreError::Poisoned)
    }
}

fn configure(conn: &Connection) -> Result<(), StoreError> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(StoreError::Corrupt(format!(
            "couldn't switch to WAL mode (got {mode})"
        )));
    }
    // An audit log: durable on every commit, even across a power cut.
    conn.pragma_update(None, "synchronous", "FULL")?;
    Ok(())
}

/// Checks every run's chain in one ordered pass. Returns the first break, if any.
fn verify(conn: &Connection) -> Result<Integrity, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM events ORDER BY run_id, seq"
    ))?;
    let rows = stmt.query_map([], Row::read)?;

    let mut current: Option<(String, u64, String)> = None; // (run, next seq, prev hash)
    for row in rows {
        let row = row?;
        let (expected_seq, expected_prev) = match &current {
            Some((run, next_seq, prev)) if *run == row.run_id => (*next_seq, prev.clone()),
            Some(_) | None => (1, GENESIS.to_owned()),
        };
        let tampered = |seq: u64, reason: String| Integrity::Tampered {
            run_id: RunId::from(row.run_id.as_str()),
            seq,
            reason,
        };

        let seq = from_db(row.seq, "seq")?;
        if seq != expected_seq {
            return Ok(tampered(
                expected_seq,
                format!("event {expected_seq} is missing (found {seq})"),
            ));
        }
        if row.prev_hash != expected_prev {
            return Ok(tampered(
                seq,
                "prev_hash doesn't match the event before it".into(),
            ));
        }
        let canonical = match row.event().and_then(|e| Ok(e.to_json()?)) {
            Ok(json) => json,
            Err(e) => return Ok(tampered(seq, format!("unreadable: {e}"))),
        };
        if chain_hash(&row.prev_hash, &canonical) != row.hash {
            return Ok(tampered(seq, "hash doesn't match the event".into()));
        }
        current = Some((row.run_id.clone(), seq + 1, row.hash.clone()));
    }
    Ok(Integrity::Verified)
}

/// SHA-256 over the previous hash, a newline, and the event's canonical JSON, as lowercase hex.
fn chain_hash(prev_hash: &str, canonical_json: &str) -> String {
    let mut h = Sha256::new();
    h.update(prev_hash.as_bytes());
    h.update(b"\n");
    h.update(canonical_json.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The event's `payload` object, as stored in `payload_json`.
fn payload_json(event: &Event) -> Result<String, StoreError> {
    let value = serde_json::to_value(event).map_err(|e| EventError::Malformed(e.to_string()))?;
    let payload = value
        .get("payload")
        .ok_or_else(|| EventError::Malformed("event has no payload".into()))?;
    Ok(payload.to_string())
}

fn to_db(n: u64, what: &str) -> Result<i64, StoreError> {
    i64::try_from(n).map_err(|_| StoreError::Corrupt(format!("{what} {n} is too large to store")))
}

fn from_db(n: i64, what: &str) -> Result<u64, StoreError> {
    u64::try_from(n).map_err(|_| StoreError::Corrupt(format!("{what} {n} is negative")))
}

/// One `events` row, as stored.
struct Row {
    run_id: String,
    seq: i64,
    ts: i64,
    kind: String,
    span_id: String,
    parent_span_id: Option<String>,
    payload_json: String,
    prev_hash: String,
    hash: String,
}

impl Row {
    fn read(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            run_id: r.get(0)?,
            seq: r.get(1)?,
            ts: r.get(2)?,
            kind: r.get(3)?,
            span_id: r.get(4)?,
            parent_span_id: r.get(5)?,
            payload_json: r.get(6)?,
            prev_hash: r.get(7)?,
            hash: r.get(8)?,
        })
    }

    /// Rebuilds the event through [`Event::from_json`], so an unknown kind or a bad payload is
    /// an error here too.
    fn event(&self) -> Result<Event, StoreError> {
        let payload: serde_json::Value = serde_json::from_str(&self.payload_json)
            .map_err(|e| EventError::Malformed(format!("payload_json: {e}")))?;
        let mut obj = serde_json::Map::new();
        obj.insert("run_id".into(), self.run_id.clone().into());
        obj.insert("seq".into(), from_db(self.seq, "seq")?.into());
        obj.insert("ts".into(), from_db(self.ts, "ts")?.into());
        obj.insert("span_id".into(), self.span_id.clone().into());
        if let Some(parent) = &self.parent_span_id {
            obj.insert("parent_span_id".into(), parent.clone().into());
        }
        obj.insert("kind".into(), self.kind.clone().into());
        obj.insert("payload".into(), payload);
        Ok(Event::from_json(
            &serde_json::Value::Object(obj).to_string(),
        )?)
    }
}
