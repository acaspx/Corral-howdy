//! The fake agent pauses between steps (TP-SUP-06): a pause asked for mid-step takes hold before
//! the next one, and Resume carries on with the rest of the script.

use corral_adapters::fake::{FakeDriver, FakeScript};
use corral_core::clock::ManualClock;
use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId};
use corral_core::lifecycle::{Lifecycle, next};
use corral_core::supervisor::{EventLog, LogError, RunRequest, Supervisor, SupervisorConfig};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::sleep;

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
}

#[tokio::test(start_paused = true)]
async fn tp_sup_06_the_fake_pauses_between_steps_and_resumes_the_rest() {
    let script = FakeScript::default()
        .say("Reading.")
        .wait(Duration::from_secs(1))
        .say("Done reading.");
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::new(
        SupervisorConfig::default(),
        Arc::new(FakeDriver::new(script)),
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    let run = RunId::from("r1");
    sup.deploy(RunRequest {
        run_id: run.clone(),
        agent_id: AgentId::from("fake"),
        session_id: SessionId::from("s1"),
        prompt: "Read my notes.".into(),
    })
    .expect("deploy");
    sleep(Duration::from_millis(500)).await;
    sup.pause(&run).expect("pause");
    sleep(Duration::from_secs(60)).await;
    let kinds = log.kinds();
    assert_eq!(kinds.last(), Some(&"run.paused"), "{kinds:?}");
    let said: Vec<String> = log
        .events()
        .iter()
        .filter_map(|e| {
            let EventBody::ModelDelta(d) = &e.body else {
                return None;
            };
            Some(d.text.clone())
        })
        .collect();
    assert_eq!(said, ["Reading."], "the second step waits");
    sup.resume(&run).expect("resume");
    tokio::time::timeout(Duration::from_secs(3600), sup.wait_idle())
        .await
        .expect("ends");
    let kinds = log.kinds();
    assert!(kinds.contains(&"run.resumed"), "{kinds:?}");
    assert_eq!(kinds.last(), Some(&"run.completed"), "{kinds:?}");
    let mut state = Lifecycle::idle();
    for e in log.events() {
        state = next(&state, &e).unwrap_or_else(|err| panic!("illegal: {err}"));
    }
}

// ---- TP-PAUSE-03: which drivers say they pause -------------------------------------------

/// A driver with no safe points: the default says it can't pause.
struct NoSafePoints;

impl corral_core::supervisor::RunDriver for NoSafePoints {
    fn run(
        &self,
        _request: RunRequest,
        _sink: corral_core::supervisor::EventSink,
        _cancel: corral_core::supervisor::CancelToken,
    ) -> corral_core::supervisor::RunFuture {
        Box::pin(async {})
    }
}

#[test]
fn tp_pause_03_the_router_and_the_switch_pause_only_when_their_drivers_do() {
    use corral_adapters::catalog::{AgentRouter, Switched};
    use corral_core::supervisor::RunDriver;
    use std::collections::HashMap;
    let grok = AgentId::from("grok");
    let fake: Arc<dyn RunDriver> = Arc::new(FakeDriver::new(FakeScript::default()));
    let none: Arc<dyn RunDriver> = Arc::new(NoSafePoints);
    assert!(fake.pauses(&grok), "the fake pauses between steps");
    assert!(!none.pauses(&grok), "the default is no");
    let router = AgentRouter::new(HashMap::from([
        (grok.clone(), Arc::clone(&fake)),
        (AgentId::from("muse"), Arc::clone(&none)),
    ]));
    assert!(router.pauses(&grok));
    assert!(!router.pauses(&AgentId::from("muse")));
    assert!(!router.pauses(&AgentId::from("nobody")), "no driver yet");
    let both = Switched::new(Arc::clone(&fake), Arc::clone(&fake), Arc::new(|| true));
    assert!(both.pauses(&grok));
    let one = Switched::new(Arc::clone(&fake), Arc::clone(&none), Arc::new(|| true));
    assert!(!one.pauses(&grok), "either build may take the next run");
    let other = Switched::new(Arc::clone(&none), Arc::clone(&fake), Arc::new(|| true));
    assert!(!other.pauses(&grok));
}
