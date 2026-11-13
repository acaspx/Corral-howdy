//! The FakeAdapter (M1 item 6): a scripted agent for testing the core without a model, and
//! Recall-all measured against it (EV-STOP-01, TP-SUP-02). Time is tokio's paused clock, so
//! waits and the 2-second force-drop are exact.

use corral_adapters::fake::{FakeDriver, FakeScript, FakeStep};
use corral_core::budget::{BudgetConfig, Price};
use corral_core::clock::ManualClock;
use corral_core::controls::{ControlError, CorralControls};
use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId};
use corral_core::lifecycle::{Lifecycle, next};
use corral_core::supervisor::{EventLog, LogError, RunRequest, Supervisor, SupervisorConfig};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;

#[derive(Default)]
struct MemLog(Mutex<Vec<Event>>);

impl EventLog for MemLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        self.0.lock().expect("log lock").push(event.clone());
        Ok(())
    }
}

impl MemLog {
    fn run(&self, run: &str) -> Vec<Event> {
        self.0
            .lock()
            .expect("log lock")
            .iter()
            .filter(|e| e.run_id.as_str() == run)
            .cloned()
            .collect()
    }

    fn kinds(&self, run: &str) -> Vec<&'static str> {
        self.run(run).iter().map(|e| e.kind().as_str()).collect()
    }

    fn terminal(&self, run: &str) -> Event {
        self.run(run).last().cloned().expect("events")
    }

    fn assert_legal(&self, run: &str) {
        let mut state = Lifecycle::idle();
        for e in self.run(run) {
            state = next(&state, &e).unwrap_or_else(|err| panic!("{run}: illegal event: {err}"));
        }
    }
}

fn supervisor(driver: FakeDriver, cap: Option<u64>) -> (Supervisor, Arc<MemLog>) {
    let log = Arc::new(MemLog::default());
    let budget = cap.map_or_else(BudgetConfig::default, |cap| BudgetConfig {
        per_run_cap_micro_usd: cap,
    });
    let sup = Supervisor::with_budget(
        SupervisorConfig {
            max_concurrent: 8,
            force_drop_after: Duration::from_secs(2),
        },
        budget,
        Arc::new(driver),
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    (sup, log)
}

fn request(run: &str, prompt: &str) -> RunRequest {
    RunRequest {
        run_id: RunId::from(run),
        agent_id: AgentId::from("fake"),
        session_id: SessionId::from(format!("s-{run}").as_str()),
        prompt: prompt.into(),
    }
}

async fn idle(sup: &Supervisor) {
    tokio::time::timeout(Duration::from_secs(600), sup.wait_idle())
        .await
        .expect("runs end");
}

/// Lets spawned tasks run until nothing is left to do right now.
async fn settle() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

fn failure(log: &MemLog, run: &str) -> RunFailed {
    let EventBody::RunFailed(f) = log.terminal(run).body else {
        panic!("expected run.failed, got {:?}", log.terminal(run).kind());
    };
    f
}

// ---- TP-FAKE-01: a scripted answer ---------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_fake_01_a_scripted_answer_streams_and_completes() {
    let script = FakeScript::default()
        .say("Hello")
        .wait(Duration::from_secs(3))
        .say(" there.")
        .usage(10, 4, 30)
        .usage(2, 1, 5);
    let (sup, log) = supervisor(FakeDriver::new(script), None);
    sup.deploy(request("r1", "hi")).expect("deploy");
    let start = Instant::now();
    idle(&sup).await;
    assert_eq!(start.elapsed(), Duration::from_secs(3), "the wait is real");
    assert_eq!(
        log.kinds("r1"),
        [
            "run.queued",
            "run.started",
            "model.request",
            "model.delta",
            "model.delta",
            "model.response",
            "usage.reported",
            "run.completed",
        ]
    );
    log.assert_legal("r1");
    let events = log.run("r1");
    let EventBody::ModelRequest(req) = &events[2].body else {
        panic!("model.request");
    };
    assert_eq!(req.provider, "fake");
    let EventBody::ModelResponse(resp) = &events[5].body else {
        panic!("model.response");
    };
    assert_eq!(resp.text, "Hello there.");
    let EventBody::UsageReported(u) = &events[6].body else {
        panic!("usage.reported");
    };
    assert_eq!(
        (u.input_tokens, u.output_tokens, u.cost_micro_usd),
        (12, 5, 35)
    );
}

#[tokio::test(start_paused = true)]
async fn tp_fake_01_scripts_are_chosen_by_prompt_and_runs_are_recorded() {
    let driver = FakeDriver::new(FakeScript::default().say("default"))
        .with_prompt("special", FakeScript::default().say("special answer"));
    let seen = driver.runs();
    let (sup, log) = supervisor(driver, None);
    sup.deploy(request("a", "anything")).expect("deploy");
    sup.deploy(request("b", "special")).expect("deploy");
    idle(&sup).await;
    let answer = |run: &str| {
        log.run(run)
            .into_iter()
            .find_map(|e| {
                let EventBody::ModelResponse(r) = e.body else {
                    return None;
                };
                Some(r.text)
            })
            .expect("an answer")
    };
    assert_eq!(answer("a"), "default");
    assert_eq!(answer("b"), "special answer");
    let prompts: Vec<String> = seen
        .lock()
        .expect("seen")
        .iter()
        .map(|r| r.prompt.clone())
        .collect();
    assert_eq!(prompts, ["anything", "special"]);
}

// ---- TP-FAKE-02: Stop ----------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_fake_02_a_wait_ends_at_once_on_stop() {
    let script = FakeScript::default()
        .say("thinking")
        .wait(Duration::from_secs(3600))
        .say("never said");
    let (sup, log) = supervisor(FakeDriver::new(script), None);
    sup.deploy(request("r1", "hi")).expect("deploy");
    settle().await;
    let start = Instant::now();
    sup.cancel(&RunId::from("r1")).await.expect("cancel");
    assert!(
        start.elapsed() < Duration::from_millis(10),
        "no force-drop needed"
    );
    idle(&sup).await;
    assert_eq!(log.terminal("r1").kind().as_str(), "run.cancelled");
    assert!(!log.kinds("r1").contains(&"model.response"));
    log.assert_legal("r1");
}

#[tokio::test(start_paused = true)]
async fn tp_fake_02_a_stubborn_step_ignores_stop_and_is_dropped_at_two_seconds() {
    let script = FakeScript::default().stubborn(Duration::from_secs(3600));
    let (sup, log) = supervisor(FakeDriver::new(script), None);
    sup.deploy(request("r1", "hi")).expect("deploy");
    settle().await;
    let start = Instant::now();
    sup.cancel(&RunId::from("r1")).await.expect("cancel");
    assert_eq!(start.elapsed(), Duration::from_secs(2));
    assert_eq!(log.terminal("r1").kind().as_str(), "run.cancelled");
}

// ---- TP-FAKE-03: tools and failures -----------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_fake_03_a_tool_step_fails_the_run_until_tools_exist() {
    let script = FakeScript::default()
        .say("I'll send that now.")
        .tool(
            "email.send",
            serde_json::json!({"to": "someone@example.test"}),
        )
        .say("never said");
    let (sup, log) = supervisor(FakeDriver::new(script), None);
    sup.deploy(request("r1", "hi")).expect("deploy");
    idle(&sup).await;
    let f = failure(&log, "r1");
    assert_eq!(f.reason, FailureReason::ToolsNotEnabled);
    assert!(f.message.contains("email.send"), "{}", f.message);
    let kinds = log.kinds("r1");
    assert!(!kinds.iter().any(|k| k.starts_with("tool.")), "{kinds:?}");
    log.assert_legal("r1");
}

#[tokio::test(start_paused = true)]
async fn tp_fake_03_a_scripted_failure_ends_the_run_with_its_reason() {
    let script = FakeScript::default().fail(FailureReason::Provider, "The provider is down.");
    let (sup, log) = supervisor(FakeDriver::new(script), None);
    sup.deploy(request("r1", "hi")).expect("deploy");
    idle(&sup).await;
    let f = failure(&log, "r1");
    assert_eq!(f.reason, FailureReason::Provider);
    assert_eq!(f.message, "The provider is down.");
}

#[tokio::test(start_paused = true)]
async fn tp_fake_03_a_priced_fake_goes_through_the_budget_meter() {
    // At $10 per million tokens, the prompt's bounded input (258 tokens) alone costs 2,580 µ$,
    // more than the 100 µ$ cap, so the meter refuses and nothing is "sent".
    let driver = FakeDriver::new(FakeScript::default().say("too expensive"))
        .priced(Price::per_million_tokens(10_000_000, 10_000_000));
    let (sup, log) = supervisor(driver, Some(100));
    sup.deploy(request("r1", "hi")).expect("deploy");
    idle(&sup).await;
    assert_eq!(failure(&log, "r1").reason, FailureReason::BudgetHardCap);
    assert!(!log.kinds("r1").contains(&"model.delta"));
}

// ---- TP-FAKE-04: steps are data -------------------------------------------------------------------

#[test]
fn tp_fake_04_a_script_lists_its_steps_in_order() {
    let script = FakeScript::default()
        .say("a")
        .wait(Duration::from_millis(5))
        .tool("http.get", serde_json::json!({}));
    assert_eq!(
        script.steps(),
        [
            FakeStep::Say("a".into()),
            FakeStep::Wait(Duration::from_millis(5)),
            FakeStep::Tool {
                name: "http.get".into(),
                args: serde_json::json!({})
            },
        ]
    );
}

// ---- EV-STOP-01 / TP-SUP-02: Recall-all with 8 fake runs -----------------------------------------

#[tokio::test(start_paused = true)]
async fn ev_stop_01_recall_all_stops_eight_fake_runs_within_two_seconds() {
    let working = FakeScript::default()
        .say("working")
        .wait(Duration::from_secs(3600));
    let driver = FakeDriver::new(working).with_prompt(
        "stubborn",
        FakeScript::default().stubborn(Duration::from_secs(3600)),
    );
    let (sup, log) = supervisor(driver, None);
    for i in 0..7 {
        sup.deploy(request(&format!("r{i}"), "work"))
            .expect("deploy");
    }
    sup.deploy(request("r7", "stubborn")).expect("deploy");
    settle().await;
    assert_eq!(sup.running(), 8);

    let start = Instant::now();
    let report = sup.recall_all().await;
    let took = start.elapsed();
    assert_eq!(report.stopped, 8);
    assert_eq!(report.forced, 1, "only the stubborn one had to be dropped");
    assert!(took <= Duration::from_secs(2), "Recall-all took {took:?}");
    for i in 0..8 {
        let run = format!("r{i}");
        assert_eq!(log.terminal(&run).kind().as_str(), "run.cancelled", "{run}");
        log.assert_legal(&run);
    }
}

#[tokio::test(start_paused = true)]
async fn ev_stop_01_with_well_behaved_runs_recall_all_is_immediate() {
    let driver = FakeDriver::new(FakeScript::default().wait(Duration::from_secs(3600)));
    let (sup, _log) = supervisor(driver, None);
    for i in 0..8 {
        sup.deploy(request(&format!("r{i}"), "work"))
            .expect("deploy");
    }
    settle().await;
    let start = Instant::now();
    let report = sup.recall_all().await;
    assert_eq!((report.stopped, report.forced), (8, 0));
    assert!(start.elapsed() < Duration::from_millis(10));
}

// ---- TP-SUP-02 from the corral: Stop all agents -------------------------------------------------

fn counter() -> Arc<dyn Fn() -> String + Send + Sync> {
    let n = Arc::new(AtomicU64::new(0));
    Arc::new(move || format!("{}", n.fetch_add(1, Ordering::SeqCst) + 1))
}

#[tokio::test(start_paused = true)]
async fn tp_sup_02_stop_all_stops_every_working_agent_and_each_can_work_again() {
    let driver = FakeDriver::new(FakeScript::default().say("ok")).with_prompt(
        "long",
        FakeScript::default().wait(Duration::from_secs(3600)),
    );
    let (sup, log) = supervisor(driver, None);
    let a = AgentId::from("grok");
    let b = AgentId::from("claude");
    let c = AgentId::from("gpt");
    let controls = CorralControls::new(sup.clone(), [a.clone(), b.clone(), c.clone()], counter());
    let ra = controls.deploy(&a, "long").expect("deploy a");
    let rb = controls.deploy(&b, "long").expect("deploy b");
    let rc = controls.deploy(&c, "quick").expect("deploy c");
    settle().await;
    assert_eq!(log.terminal(rc.as_str()).kind().as_str(), "run.completed");

    let report = controls.stop_all().await;
    assert_eq!(report.stopped, 2, "only the working ones");
    for run in [&ra, &rb] {
        let EventBody::RunCancelled(c) = log.terminal(run.as_str()).body else {
            panic!("{run} should be cancelled");
        };
        assert_eq!(c.by, CancelledBy::RecallAll);
    }
    assert_eq!(
        log.terminal(rc.as_str()).kind().as_str(),
        "run.completed",
        "a done agent is left alone"
    );

    // Nothing is working now; each agent takes a new task in a new session.
    assert_eq!(controls.stop_all().await.stopped, 0);
    let again = controls.deploy(&a, "quick").expect("deploy again");
    idle(&sup).await;
    assert_eq!(
        log.terminal(again.as_str()).kind().as_str(),
        "run.completed"
    );
    assert_eq!(
        controls.deploy(&AgentId::from("stranger"), "x"),
        Err(ControlError::UnknownAgent(AgentId::from("stranger")))
    );
}
