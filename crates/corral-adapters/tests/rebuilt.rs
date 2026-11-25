//! An agent rebuilt for the settings it has when each run starts (M4 item 6, TP-PED-07): a saved
//! policy applies from the agent's next run, without a restart. `Rebuilt` asks for the settings'
//! version as a run starts, builds the agent again only when it changed, and leaves runs already
//! going with the driver they started on.

use corral_adapters::catalog::Rebuilt;
use corral_adapters::fake::{FakeDriver, FakeScript};
use corral_core::clock::ManualClock;
use corral_core::event::{Event, EventBody};
use corral_core::ids::{AgentId, RunId, SessionId};
use corral_core::supervisor::{
    CancelToken, EventLog, EventSink, LogError, RunDriver, RunFuture, RunRequest, Supervisor,
    SupervisorConfig,
};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
    fn said(&self) -> Vec<String> {
        let events = self.0.lock().expect("log");
        events
            .iter()
            .filter_map(|e| {
                let EventBody::ModelDelta(d) = &e.body else {
                    return None;
                };
                Some(d.text.clone())
            })
            .collect()
    }
}

/// A driver with no safe points.
struct Plain;

impl RunDriver for Plain {
    fn run(&self, _request: RunRequest, _sink: EventSink, _cancel: CancelToken) -> RunFuture {
        Box::pin(async {})
    }
}

/// Settings at `version`, and a builder that counts its builds and says which version it saw.
struct Settings {
    version: Arc<AtomicU64>,
    builds: Arc<AtomicUsize>,
}

impl Settings {
    fn new() -> Self {
        Self {
            version: Arc::new(AtomicU64::new(1)),
            builds: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn rebuilt(&self, slow: bool) -> Rebuilt {
        let version = Arc::clone(&self.version);
        let seen = Arc::clone(&self.version);
        let builds = Arc::clone(&self.builds);
        Rebuilt::new(
            Arc::new(move || version.load(Ordering::SeqCst)),
            Arc::new(move || {
                builds.fetch_add(1, Ordering::SeqCst);
                let v = seen.load(Ordering::SeqCst);
                let mut script = FakeScript::default().say(format!("built for v{v}"));
                if slow {
                    script = script
                        .wait(Duration::from_secs(60))
                        .say(format!("still v{v}"));
                }
                Arc::new(FakeDriver::new(script)) as Arc<dyn RunDriver>
            }),
        )
    }

    fn bump(&self) {
        self.version.fetch_add(1, Ordering::SeqCst);
    }

    fn builds(&self) -> usize {
        self.builds.load(Ordering::SeqCst)
    }
}

fn supervisor(driver: Rebuilt) -> (Supervisor, Arc<MemLog>) {
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::new(
        SupervisorConfig::default(),
        Arc::new(driver),
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    (sup, log)
}

fn deploy(sup: &Supervisor, run: &str) {
    sup.deploy(RunRequest {
        run_id: RunId::from(run),
        agent_id: AgentId::from("grok"),
        session_id: SessionId::from(format!("s-{run}").as_str()),
        prompt: "Tidy my notes.".into(),
    })
    .expect("deploy");
}

async fn idle(sup: &Supervisor) {
    tokio::time::timeout(Duration::from_secs(3600), sup.wait_idle())
        .await
        .expect("idle");
}

#[tokio::test(start_paused = true)]
async fn tp_ped_07_a_saved_change_applies_from_the_next_run() {
    let settings = Settings::new();
    let (sup, log) = supervisor(settings.rebuilt(false));
    deploy(&sup, "r1");
    idle(&sup).await;
    deploy(&sup, "r2");
    idle(&sup).await;
    assert_eq!(settings.builds(), 1, "built once while nothing changed");
    settings.bump();
    deploy(&sup, "r3");
    idle(&sup).await;
    assert_eq!(settings.builds(), 2, "rebuilt for the change");
    assert_eq!(log.said(), ["built for v1", "built for v1", "built for v2"]);
}

#[tokio::test(start_paused = true)]
async fn tp_ped_07_a_run_already_going_keeps_the_driver_it_started_on() {
    let settings = Settings::new();
    let (sup, log) = supervisor(settings.rebuilt(true));
    deploy(&sup, "r1");
    tokio::time::sleep(Duration::from_secs(1)).await;
    settings.bump();
    deploy(&sup, "r2");
    idle(&sup).await;
    let mut said = log.said();
    said.sort();
    assert_eq!(
        said,
        ["built for v1", "built for v2", "still v1", "still v2"],
        "r1 finished on v1"
    );
}

#[test]
fn tp_ped_07_whether_it_pauses_is_the_current_builds_answer() {
    let version = Arc::new(AtomicU64::new(1));
    let v = Arc::clone(&version);
    let rebuilt = Rebuilt::new(
        Arc::new(move || v.load(Ordering::SeqCst)),
        Arc::new({
            let version = Arc::clone(&version);
            move || {
                if version.load(Ordering::SeqCst) == 1 {
                    Arc::new(FakeDriver::new(FakeScript::default())) as Arc<dyn RunDriver>
                } else {
                    Arc::new(Plain) as Arc<dyn RunDriver>
                }
            }
        }),
    );
    let grok = AgentId::from("grok");
    assert!(rebuilt.pauses(&grok));
    version.store(2, Ordering::SeqCst);
    assert!(!rebuilt.pauses(&grok), "the rebuilt driver can't pause");
}
