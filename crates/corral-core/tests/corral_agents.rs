//! The corral follows the agents you have (TP-VIEW-06): connecting a provider's key adds its
//! agent's drop, and removing the key takes it away, without losing what a run is doing.

use corral_core::clock::ManualClock;
use corral_core::controls::{ControlError, CorralControls};
use corral_core::corral_view::{AgentCard, CorralTracker, DropState};
use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId, SpanId};
use corral_core::supervisor::{
    CancelToken, EventLog, EventSink, LogError, RunDriver, RunFuture, RunRequest, Supervisor,
    SupervisorConfig,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

fn card(id: &str, name: &str) -> AgentCard {
    AgentCard {
        id: AgentId::from(id),
        name: name.into(),
        monogram: name[..1].into(),
    }
}

fn ids(tracker: &CorralTracker) -> Vec<String> {
    tracker
        .view()
        .drops
        .iter()
        .map(|d| d.agent.id.to_string())
        .collect()
}

fn ev(run: &str, body: EventBody) -> Event {
    Event {
        run_id: RunId::from(run),
        seq: 1,
        ts: 0,
        span_id: SpanId::from("span"),
        parent_span_id: None,
        body,
    }
}

#[test]
fn tp_view_06_a_new_agent_drops_in_ready_and_a_removed_one_leaves() {
    let mut t = CorralTracker::new(vec![card("grok", "Grok")]);
    assert!(t.set_agents(vec![card("grok", "Grok"), card("claude", "Claude")]));
    assert_eq!(ids(&t), ["grok", "claude"]);
    assert_eq!(t.view().drops[1].state, DropState::Available);
    assert!(
        !t.set_agents(vec![card("grok", "Grok"), card("claude", "Claude")]),
        "nothing changed"
    );
    assert!(t.set_agents(vec![card("claude", "Claude")]));
    assert_eq!(ids(&t), ["claude"]);
}

#[test]
fn tp_view_06_a_working_agent_keeps_its_state_across_removal_and_return() {
    let mut t = CorralTracker::new(vec![card("grok", "Grok")]);
    t.observe(&ev(
        "r1",
        EventBody::RunQueued(RunQueued {
            agent_id: AgentId::from("grok"),
            session_id: SessionId::from("s1"),
            prompt: "hi".into(),
        }),
    ))
    .expect("queued");
    t.set_agents(Vec::new());
    assert!(t.view().drops.is_empty());
    // Its run is still tracked: events keep applying while it's hidden.
    t.observe(&ev(
        "r1",
        EventBody::RunStarted(RunStarted {
            adapter: "xai".into(),
        }),
    ))
    .expect("started");
    t.set_agents(vec![card("grok", "Grok")]);
    assert_eq!(t.view().drops[0].state, DropState::Working);
}

#[test]
fn tp_view_06_a_repeated_agent_keeps_its_first_card() {
    let mut t = CorralTracker::new(Vec::new());
    t.set_agents(vec![card("grok", "Grok"), card("grok", "Other")]);
    assert_eq!(t.view().drops.len(), 1);
    assert_eq!(t.view().drops[0].agent.name, "Grok");
}

// ---- Controls take tasks only for agents in the corral ----------------------------------------

#[derive(Default)]
struct NullLog(Mutex<Vec<Event>>);

impl EventLog for NullLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        self.0.lock().expect("lock").push(event.clone());
        Ok(())
    }
}

struct Done;

impl RunDriver for Done {
    fn run(&self, _request: RunRequest, sink: EventSink, _cancel: CancelToken) -> RunFuture {
        Box::pin(async move {
            sink.emit(EventBody::RunCompleted(RunCompleted { summary: None }))
                .expect("completed");
        })
    }
}

#[tokio::test(start_paused = true)]
async fn tp_view_06_controls_follow_the_agents_in_the_corral() {
    let sup = Supervisor::new(
        SupervisorConfig::default(),
        Arc::new(Done),
        Arc::new(NullLog::default()),
        Arc::new(ManualClock::at(1_000)),
    );
    let n = Arc::new(AtomicU64::new(0));
    let controls = CorralControls::new(
        sup,
        [AgentId::from("grok")],
        Arc::new(move || n.fetch_add(1, Ordering::SeqCst).to_string()),
    );
    let claude = AgentId::from("claude");
    assert_eq!(
        controls.deploy(&claude, "hi"),
        Err(ControlError::UnknownAgent(claude.clone()))
    );
    controls.set_agents([AgentId::from("grok"), claude.clone()]);
    assert!(controls.deploy(&claude, "hi").is_ok());
    controls.set_agents([claude.clone()]);
    assert_eq!(
        controls.deploy(&AgentId::from("grok"), "hi"),
        Err(ControlError::UnknownAgent(AgentId::from("grok")))
    );
}
