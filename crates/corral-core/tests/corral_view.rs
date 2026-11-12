//! The corral view: TP-VIEW-01..05 (`docs/04-test-plan.md`, SK-2b). What each drop shows is
//! decided here in the core from the event log; Swift only draws it (AGENTS.md rule 3).

use corral_core::corral_view::{AgentCard, CorralTracker, DropState, MAX_SHOWN, ViewError};
use corral_core::domain::{ActionClass, Risk};
use corral_core::event::*;
use corral_core::ids::{AgentId, CheckpointId, RunId, SessionId, SpanId, ToolCallId};
use corral_core::supervisor::{EventLog, LogError, WatchedLog};
use std::sync::{Arc, Mutex};

// ---- Building events ------------------------------------------------------------------------

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

fn queued(run: &str, agent: &str) -> Event {
    ev(
        run,
        EventBody::RunQueued(RunQueued {
            agent_id: AgentId::from(agent),
            session_id: SessionId::from(format!("session-{run}")),
            prompt: "a task".into(),
        }),
    )
}

fn started(run: &str) -> Event {
    ev(
        run,
        EventBody::RunStarted(RunStarted {
            adapter: "fake".into(),
        }),
    )
}

fn delta(run: &str) -> Event {
    ev(run, EventBody::ModelDelta(ModelDelta { text: "hi".into() }))
}

fn opened(run: &str, cp: &str) -> Event {
    ev(
        run,
        EventBody::CheckpointOpened(CheckpointOpened {
            checkpoint_id: CheckpointId::from(cp),
            tool_call_id: ToolCallId::from("call"),
            action_class: ActionClass::CommsSend,
            risk: Risk::High,
            title: "Send an email".into(),
            detail: "To: someone@example.test".into(),
        }),
    )
}

fn approved(run: &str, cp: &str) -> Event {
    ev(
        run,
        EventBody::CheckpointResolved(CheckpointResolved {
            checkpoint_id: CheckpointId::from(cp),
            resolution: Resolution::Approved,
        }),
    )
}

fn paused(run: &str) -> Event {
    ev(
        run,
        EventBody::RunPaused(RunPaused {
            reason: PauseReason::User,
        }),
    )
}

fn completed(run: &str) -> Event {
    ev(run, EventBody::RunCompleted(RunCompleted { summary: None }))
}

fn failed(run: &str) -> Event {
    ev(
        run,
        EventBody::RunFailed(RunFailed {
            reason: FailureReason::Provider,
            message: "provider error".into(),
        }),
    )
}

fn cancelled(run: &str) -> Event {
    ev(
        run,
        EventBody::RunCancelled(RunCancelled {
            by: CancelledBy::User,
        }),
    )
}

fn closed(run: &str) -> Event {
    ev(run, EventBody::SessionClosed(SessionClosed {}))
}

fn card(id: &str) -> AgentCard {
    AgentCard {
        id: AgentId::from(id),
        name: id.to_uppercase(),
        monogram: id[..1].to_uppercase(),
    }
}

fn tracker(agents: &[&str]) -> CorralTracker {
    CorralTracker::new(agents.iter().map(|a| card(a)).collect())
}

fn feed(t: &mut CorralTracker, events: &[Event]) {
    for e in events {
        t.observe(e).expect("the lifecycle accepts the event");
    }
}

/// The state of `agent`'s drop.
fn state(t: &CorralTracker, agent: &str) -> DropState {
    t.view()
        .drops
        .iter()
        .find(|d| d.agent.id == AgentId::from(agent))
        .map(|d| d.state)
        .expect("the agent is shown")
}

// ---- TP-VIEW-01: drop status from run phases ------------------------------------------------

#[test]
fn tp_view_01_a_new_agent_is_available() {
    assert_eq!(state(&tracker(&["scout"]), "scout"), DropState::Available);
}

#[test]
fn tp_view_01_each_phase_shows_its_drop_status() {
    let cases: Vec<(Vec<Event>, DropState)> = vec![
        (vec![queued("r", "scout")], DropState::Working),
        (vec![queued("r", "scout"), started("r")], DropState::Working),
        (
            vec![queued("r", "scout"), started("r"), paused("r")],
            DropState::Working,
        ),
        (
            vec![queued("r", "scout"), started("r"), opened("r", "cp")],
            DropState::NeedsYou,
        ),
        (
            vec![
                queued("r", "scout"),
                started("r"),
                opened("r", "cp"),
                approved("r", "cp"),
            ],
            DropState::Working,
        ),
        (
            vec![queued("r", "scout"), started("r"), completed("r")],
            DropState::Done,
        ),
        (
            vec![queued("r", "scout"), started("r"), failed("r")],
            DropState::Error,
        ),
        (
            vec![queued("r", "scout"), started("r"), cancelled("r")],
            DropState::Available,
        ),
    ];
    for (events, expected) in cases {
        let mut t = tracker(&["scout"]);
        feed(&mut t, &events);
        let kinds: Vec<_> = events.iter().map(Event::kind).collect();
        assert_eq!(state(&t, "scout"), expected, "after {kinds:?}");
    }
}

// ---- TP-VIEW-02: several runs, and Send back -------------------------------------------------

#[test]
fn tp_view_02_needs_you_beats_working_beats_the_last_outcome() {
    let mut t = tracker(&["scout"]);
    feed(
        &mut t,
        &[queued("a", "scout"), started("a"), completed("a")],
    );
    assert_eq!(state(&t, "scout"), DropState::Done);

    feed(&mut t, &[queued("b", "scout"), started("b")]);
    assert_eq!(state(&t, "scout"), DropState::Working);

    feed(
        &mut t,
        &[queued("c", "scout"), started("c"), opened("c", "cp")],
    );
    assert_eq!(state(&t, "scout"), DropState::NeedsYou);

    // The checkpoint is answered: b and c are both still working.
    feed(&mut t, &[approved("c", "cp")]);
    assert_eq!(state(&t, "scout"), DropState::Working);

    // Both finish; the most recently deployed run's outcome shows.
    feed(&mut t, &[completed("c"), failed("b")]);
    assert_eq!(state(&t, "scout"), DropState::Done);
}

#[test]
fn tp_view_02_send_back_returns_the_drop_to_available() {
    let mut t = tracker(&["scout"]);
    feed(
        &mut t,
        &[
            queued("r", "scout"),
            started("r"),
            completed("r"),
            closed("r"),
        ],
    );
    assert_eq!(state(&t, "scout"), DropState::Available);
}

#[test]
fn tp_view_02_one_agents_runs_dont_touch_another_drop() {
    let mut t = tracker(&["scout", "ranger"]);
    feed(
        &mut t,
        &[queued("r", "scout"), started("r"), opened("r", "cp")],
    );
    assert_eq!(state(&t, "scout"), DropState::NeedsYou);
    assert_eq!(state(&t, "ranger"), DropState::Available);
}

// ---- TP-VIEW-03: change detection -----------------------------------------------------------

#[test]
fn tp_view_03_only_drop_changes_are_reported() {
    let mut t = tracker(&["scout"]);
    assert!(
        t.observe(&queued("r", "scout")).unwrap(),
        "available -> working"
    );
    assert!(!t.observe(&started("r")).unwrap(), "still working");
    for _ in 0..50 {
        assert!(
            !t.observe(&delta("r")).unwrap(),
            "streamed text doesn't republish"
        );
    }
    assert!(
        t.observe(&opened("r", "cp")).unwrap(),
        "working -> needs you"
    );
    assert!(
        t.observe(&approved("r", "cp")).unwrap(),
        "needs you -> working"
    );
    assert!(t.observe(&completed("r")).unwrap(), "working -> done");
}

#[test]
fn tp_view_03_runs_of_agents_not_in_the_corral_change_nothing() {
    let mut t = tracker(&["scout"]);
    assert!(!t.observe(&queued("r", "stranger")).unwrap());
    assert!(!t.observe(&started("r")).unwrap());
    assert_eq!(state(&t, "scout"), DropState::Available);
}

// ---- TP-VIEW-04: shown set and refusals -----------------------------------------------------

#[test]
fn tp_view_04_the_first_eight_agents_show_in_the_order_added() {
    let ids: Vec<String> = (1..=11).map(|i| format!("agent{i}")).collect();
    let names: Vec<&str> = ids.iter().map(String::as_str).collect();
    let view = tracker(&names).view();
    let shown: Vec<String> = view.drops.iter().map(|d| d.agent.id.to_string()).collect();
    assert_eq!(shown, ids[..MAX_SHOWN].to_vec());
    assert_eq!(view.hidden, 3);
}

#[test]
fn tp_view_04_duplicate_agents_are_listed_once() {
    let view = tracker(&["scout", "ranger", "scout"]).view();
    let shown: Vec<String> = view.drops.iter().map(|d| d.agent.id.to_string()).collect();
    assert_eq!(shown, ["scout", "ranger"]);
    assert_eq!(view.hidden, 0);
}

#[test]
fn tp_view_04_events_the_lifecycle_refuses_are_refused_and_change_nothing() {
    let mut t = tracker(&["scout"]);
    assert!(matches!(
        t.observe(&started("never-queued")),
        Err(ViewError::UnknownRun(_))
    ));
    feed(
        &mut t,
        &[queued("r", "scout"), started("r"), completed("r")],
    );
    assert!(matches!(
        t.observe(&opened("r", "cp")),
        Err(ViewError::Lifecycle(_))
    ));
    assert_eq!(state(&t, "scout"), DropState::Done);
}

// ---- TP-VIEW-05: WatchedLog -----------------------------------------------------------------

#[derive(Default)]
struct MemoryLog {
    events: Mutex<Vec<Event>>,
    fail: bool,
}

impl EventLog for MemoryLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        if self.fail {
            return Err(LogError("disk full".into()));
        }
        self.events
            .lock()
            .expect("not poisoned")
            .push(event.clone());
        Ok(())
    }
}

#[test]
fn tp_view_05_the_observer_sees_exactly_what_the_log_accepted_in_order() {
    let inner = Arc::new(MemoryLog::default());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let log = WatchedLog::new(inner.clone(), move |e: &Event| {
        sink.lock().unwrap().push(e.clone());
    });
    for e in [queued("r", "scout"), started("r"), completed("r")] {
        log.append(&e).unwrap();
    }
    assert_eq!(*seen.lock().unwrap(), *inner.events.lock().unwrap());
    assert_eq!(seen.lock().unwrap().len(), 3);
}

#[test]
fn tp_view_05_a_refused_append_reaches_no_observer() {
    let inner = Arc::new(MemoryLog {
        fail: true,
        ..MemoryLog::default()
    });
    let seen = Arc::new(Mutex::new(0usize));
    let count = Arc::clone(&seen);
    let log = WatchedLog::new(inner, move |_: &Event| *count.lock().unwrap() += 1);
    assert_eq!(
        log.append(&queued("r", "scout")),
        Err(LogError("disk full".into()))
    );
    assert_eq!(*seen.lock().unwrap(), 0);
}
