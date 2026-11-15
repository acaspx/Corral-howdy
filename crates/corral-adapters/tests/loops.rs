//! The loop detector end to end (M2 item 7, EV-COST-02): a FakeAdapter run that repeats the same
//! call stops at a checkpoint on its card from the `loop_limit`-th time on, even for a tool the
//! policy allows. Approve runs it; Deny tells the agent, which carries on.

use corral_adapters::fake::{FakeDriver, FakeScript, ToolSetup};
use corral_core::clock::ManualClock;
use corral_core::domain::ActionClass;
use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId};
use corral_core::lifecycle::{Lifecycle, next};
use corral_core::supervisor::{EventLog, LogError, RunRequest, Supervisor, SupervisorConfig};
use corral_policy::{Approval, Policy, Profile, Scope};
use corral_tools::{Answer, BoxFuture, CheckpointDesk, Tool, ToolAnswer, ToolError, ToolGateway};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
}

struct Reader(Arc<Mutex<Vec<Value>>>);

impl Tool for Reader {
    fn name(&self) -> &str {
        "fs.read"
    }

    fn class(&self) -> ActionClass {
        ActionClass::ReadLocal
    }

    fn call<'a>(&'a self, args: &'a Value) -> BoxFuture<'a, Result<String, ToolError>> {
        self.0.lock().expect("calls").push(args.clone());
        Box::pin(async { Ok("the same notes".to_owned()) })
    }
}

struct World {
    sup: Supervisor,
    log: Arc<MemLog>,
    desk: Arc<CheckpointDesk>,
    reads: Arc<Mutex<Vec<Value>>>,
    answers: Arc<Mutex<Vec<ToolAnswer>>>,
}

fn world() -> World {
    let mut script = FakeScript::default().say("Let me check my notes.");
    for _ in 0..5 {
        script = script.tool("fs.read", json!({ "path": "notes.md" }));
    }
    let reads = Arc::new(Mutex::new(Vec::new()));
    let mut gateway = ToolGateway::new();
    gateway
        .register(Arc::new(Reader(Arc::clone(&reads))))
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
    let sup = Supervisor::new(
        SupervisorConfig::default(),
        Arc::new(driver),
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    sup.deploy(RunRequest {
        run_id: RunId::from("r1"),
        agent_id: AgentId::from("fake"),
        session_id: SessionId::from("s1"),
        prompt: "What's in my notes?".into(),
    })
    .expect("deploy");
    World {
        sup,
        log,
        desk,
        reads,
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
    let mut state = Lifecycle::idle();
    for e in w.log.events() {
        state = next(&state, &e).unwrap_or_else(|err| panic!("illegal event: {err}"));
    }
}

#[tokio::test(start_paused = true)]
async fn ev_cost_02_the_fifth_identical_read_waits_on_the_card_and_approve_runs_it() {
    let w = world();
    settle().await;
    assert_eq!(w.reads.lock().expect("reads").len(), 4, "four ran unasked");
    let [cp] = &w.log.opened()[..] else {
        panic!("one checkpoint: {:?}", w.log.opened());
    };
    assert_eq!(cp.action_class, ActionClass::ReadLocal);
    assert_eq!(
        cp.detail,
        "Same call 5 times in this run.\nfs.read\n{\"path\":\"notes.md\"}"
    );
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
    assert_eq!(w.reads.lock().expect("reads").len(), 5);
    assert!(matches!(
        w.log.events().last().map(|e| &e.body),
        Some(EventBody::RunCompleted(_))
    ));
}

#[tokio::test(start_paused = true)]
async fn ev_cost_02_deny_tells_the_agent_and_it_carries_on() {
    let w = world();
    settle().await;
    let cp = w.log.opened().pop().expect("checkpoint");
    w.desk
        .answer(
            &cp.checkpoint_id,
            Answer::Deny {
                note: Some("You already read it.".into()),
            },
        )
        .expect("denied");
    idle(&w).await;
    assert_eq!(
        w.reads.lock().expect("reads").len(),
        4,
        "the fifth didn't run"
    );
    let answers = w.answers.lock().expect("answers").clone();
    assert!(
        matches!(answers.last(), Some(ToolAnswer::Denied { note: Some(n), .. }) if n == "You already read it."),
        "{answers:?}"
    );
    assert!(matches!(
        w.log.events().last().map(|e| &e.body),
        Some(EventBody::RunCompleted(_))
    ));
}
