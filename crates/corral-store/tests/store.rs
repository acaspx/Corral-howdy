//! Event store: TP-STORE-01, TP-STORE-04, TP-STORE-05 (`docs/04-test-plan.md`).

use corral_core::event::{Event, EventBody, ModelDelta, RunCompleted};
use corral_core::ids::{RunId, SpanId};
use corral_store::{Integrity, Store, StoreError};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A fresh database path in the system temp folder, removed (with its WAL files) on drop.
struct TempDb(PathBuf);

impl TempDb {
    fn new(name: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("corral-store-{}-{name}-{n}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).expect("clear old test folder");
        }
        std::fs::create_dir_all(&dir).expect("create test folder");
        Self(dir)
    }

    fn path(&self) -> PathBuf {
        self.0.join("corral.db")
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        // Best effort: a leftover temp folder doesn't affect the result.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn delta(run: &str, seq: u64) -> Event {
    Event {
        run_id: RunId::from(run),
        seq,
        ts: 1_000 + seq,
        span_id: SpanId::from(format!("span-{seq}").as_str()),
        parent_span_id: (seq > 1).then(|| SpanId::from("span-1")),
        body: EventBody::ModelDelta(ModelDelta {
            text: format!("{run} says {seq}, ünïcode ✓"),
        }),
    }
}

fn completed(run: &str, seq: u64) -> Event {
    Event {
        run_id: RunId::from(run),
        seq,
        ts: 2_000,
        span_id: SpanId::from("span-end"),
        parent_span_id: None,
        body: EventBody::RunCompleted(RunCompleted {
            summary: Some("done".into()),
        }),
    }
}

// ---- TP-STORE-01 ---------------------------------------------------------------------------

#[test]
fn tp_store_01_append_and_read_round_trip_in_seq_order() {
    let db = TempDb::new("roundtrip");
    let store = Store::open(&db.path()).expect("open");
    assert_eq!(store.integrity(), &Integrity::Verified);

    // Interleave two runs; each run reads back in its own seq order.
    let a: Vec<Event> = (1..=3)
        .map(|s| delta("run-a", s))
        .chain([completed("run-a", 4)])
        .collect();
    let b: Vec<Event> = (1..=2).map(|s| delta("run-b", s)).collect();
    for (x, y) in a.iter().zip(b.iter()) {
        store.append(x).expect("append a");
        store.append(y).expect("append b");
    }
    for x in &a[b.len()..] {
        store.append(x).expect("append rest of a");
    }

    assert_eq!(store.read_run(&RunId::from("run-a")).expect("read a"), a);
    assert_eq!(store.read_run(&RunId::from("run-b")).expect("read b"), b);
    assert!(
        store
            .read_run(&RunId::from("nobody"))
            .expect("read empty")
            .is_empty()
    );

    // Survives closing and reopening, and still verifies.
    drop(store);
    let reopened = Store::open(&db.path()).expect("reopen");
    assert_eq!(reopened.integrity(), &Integrity::Verified);
    assert_eq!(reopened.read_run(&RunId::from("run-a")).expect("read a"), a);
}

#[test]
fn tp_store_01_seq_must_be_gapless_and_unique() {
    let db = TempDb::new("seq");
    let store = Store::open(&db.path()).expect("open");
    store.append(&delta("r", 1)).expect("first");

    assert!(matches!(
        store.append(&delta("r", 3)),
        Err(StoreError::OutOfOrder {
            expected: 2,
            got: 3,
            ..
        })
    ));
    assert!(matches!(
        store.append(&delta("r", 1)),
        Err(StoreError::OutOfOrder {
            expected: 2,
            got: 1,
            ..
        })
    ));
    assert!(matches!(
        store.append(&delta("other", 2)),
        Err(StoreError::OutOfOrder {
            expected: 1,
            got: 2,
            ..
        })
    ));
    store.append(&delta("r", 2)).expect("next in order");
}

#[test]
fn tp_store_01_the_table_refuses_updates_and_deletes() {
    let db = TempDb::new("append-only");
    let store = Store::open(&db.path()).expect("open");
    store.append(&delta("r", 1)).expect("append");
    drop(store);

    let raw = rusqlite::Connection::open(db.path()).expect("raw open");
    assert!(
        raw.execute("UPDATE events SET ts = 0", []).is_err(),
        "UPDATE must be refused"
    );
    assert!(
        raw.execute("DELETE FROM events", []).is_err(),
        "DELETE must be refused"
    );
}

// ---- TP-STORE-04 ---------------------------------------------------------------------------

#[test]
fn tp_store_04_eight_writers_and_a_reader_without_corruption() {
    const WRITERS: usize = 8;
    const EVENTS: u64 = 60;
    let db = TempDb::new("concurrency");
    let path = db.path();
    drop(Store::open(&path).expect("create"));

    std::thread::scope(|scope| {
        for w in 0..WRITERS {
            let path = path.clone();
            scope.spawn(move || {
                let store = Store::open(&path).expect("writer open");
                let run = format!("run-{w}");
                for seq in 1..=EVENTS {
                    store.append(&delta(&run, seq)).expect("writer append");
                }
            });
        }
        // The UI's reader: reads while writers append and always sees a consistent prefix.
        let path = path.clone();
        scope.spawn(move || {
            let store = Store::open(&path).expect("reader open");
            for _ in 0..200 {
                for w in 0..WRITERS {
                    let events = store
                        .read_run(&RunId::from(format!("run-{w}")))
                        .expect("read");
                    let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
                    let expected: Vec<u64> = (1..=seqs.len() as u64).collect();
                    assert_eq!(seqs, expected, "a reader sees a gapless prefix");
                }
            }
        });
    });

    let store = Store::open(&path).expect("reopen");
    assert_eq!(
        store.integrity(),
        &Integrity::Verified,
        "every chain verifies"
    );
    for w in 0..WRITERS {
        let events = store
            .read_run(&RunId::from(format!("run-{w}")))
            .expect("read");
        assert_eq!(events.len() as u64, EVENTS);
    }
}

// ---- TP-STORE-05 ---------------------------------------------------------------------------

/// Writes two runs, then tampers with the database directly, the way an attacker or a bad disk
/// would (the append-only triggers have to go first).
fn tampered(name: &str, sql: &str) -> TempDb {
    let db = TempDb::new(name);
    let store = Store::open(&db.path()).expect("open");
    for seq in 1..=3 {
        store.append(&delta("run-a", seq)).expect("append a");
        store.append(&delta("run-b", seq)).expect("append b");
    }
    drop(store);

    let raw = rusqlite::Connection::open(db.path()).expect("raw open");
    raw.execute_batch(&format!(
        "DROP TRIGGER IF EXISTS events_no_update; DROP TRIGGER IF EXISTS events_no_delete; {sql}"
    ))
    .expect("tamper");
    db
}

#[test]
fn tp_store_05_an_edited_event_fails_verification_and_opens_read_only() {
    let db = tampered(
        "edited",
        r#"UPDATE events SET payload_json = '{"text":"I never said that"}' WHERE run_id = 'run-b' AND seq = 2;"#,
    );
    let store = Store::open(&db.path()).expect("a tampered store still opens");

    match store.integrity() {
        Integrity::Tampered { run_id, seq, .. } => {
            assert_eq!(run_id, &RunId::from("run-b"));
            assert_eq!(*seq, 2);
        }
        Integrity::Verified => panic!("tampering went unnoticed"),
    }
    assert!(store.is_read_only());
    assert!(matches!(
        store.append(&delta("run-a", 4)),
        Err(StoreError::ReadOnly)
    ));
    // History stays readable, so the user can see what happened.
    assert_eq!(
        store.read_run(&RunId::from("run-a")).expect("read").len(),
        3
    );
}

#[test]
fn tp_store_05_a_deleted_event_or_a_forged_hash_is_caught_too() {
    let gap = tampered(
        "deleted",
        "DELETE FROM events WHERE run_id = 'run-a' AND seq = 2;",
    );
    assert!(matches!(
        Store::open(&gap.path()).expect("open").integrity(),
        Integrity::Tampered { seq: 2, .. }
    ));

    let forged = tampered(
        "forged",
        "UPDATE events SET ts = 9, hash = 'f00d' WHERE run_id = 'run-a' AND seq = 3;",
    );
    assert!(matches!(
        Store::open(&forged.path()).expect("open").integrity(),
        Integrity::Tampered { seq: 3, .. }
    ));
}

#[test]
fn tp_store_05_the_banner_names_the_run_and_event() {
    let db = tampered(
        "banner",
        "UPDATE events SET ts = 1 WHERE run_id = 'run-a' AND seq = 1;",
    );
    let store = Store::open(&db.path()).expect("open");
    let banner = store
        .integrity()
        .banner()
        .expect("a tampered store has a banner");
    assert!(
        banner.contains("run-a") && banner.contains("event 1"),
        "banner: {banner}"
    );
    assert_eq!(Integrity::Verified.banner(), None);
}
