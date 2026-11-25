//! Pause and Resume on the card (docs/07 §8, TP-PAUSE-01..03). A working card offers Pause only
//! for an agent whose driver stops at safe points; once pressed the card reads Pausing, with
//! Resume to take it back, until the run reaches its safe point and reads Paused. A paused drop
//! has its own state in the corral. The controls refuse Pause for an agent that can't.

use corral_core::clock::ManualClock;
use corral_core::controls::{ControlError, CorralControls};
use corral_core::corral_view::{AgentCard, CorralTracker, DropState};
use corral_core::domain::{ActionClass, Risk};
use corral_core::event::*;
use corral_core::ids::{AgentId, CheckpointId, RunId, SessionId, SpanId, ToolCallId};
use corral_core::lifecycle::Phase;
use corral_core::run_card::{CardAction, CardStatus, RunCards};
use corral_core::supervisor::{
    CancelToken, EventLog, EventSink, LogError, RunDriver, RunFuture, RunRequest, Supervisor,
    SupervisorConfig,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::sleep;

fn ev(run: &str, body: EventBody) -> Event {
    Event {
        run_id: RunId::from(run),
        seq: 1,
        ts: 1_000,
        span_id: SpanId::from("span"),
        parent_span_id: None,
        body,
    }
}

fn scout() -> AgentId {
    AgentId::from("scout")
}

fn working(views: &mut (RunCards, CorralTracker), run: &str) {
    for body in [
        EventBody::RunQueued(RunQueued {
            agent_id: scout(),
            session_id: SessionId::from(format!("s-{run}").as_str()),
            prompt: "Tidy my notes.".into(),
        }),
        EventBody::RunStarted(RunStarted {
            adapter: "fake".into(),
        }),
    ] {
        see(views, run, body);
    }
}

fn see(views: &mut (RunCards, CorralTracker), run: &str, body: EventBody) {
    let event = ev(run, body);
    views.0.observe(&event).expect("legal");
    views.1.observe(&event).expect("legal");
}

fn paused(views: &mut (RunCards, CorralTracker), run: &str) {
    see(
        views,
        run,
        EventBody::RunPaused(RunPaused {
            reason: PauseReason::User,
        }),
    );
}

fn resumed(views: &mut (RunCards, CorralTracker), run: &str) {
    see(views, run, EventBody::RunResumed(RunResumed {}));
}

fn views(pausable: bool) -> (RunCards, CorralTracker) {
    let mut cards = RunCards::default();
    if pausable {
        cards.set_pausable([scout()]);
    }
    let tracker = CorralTracker::new(vec![AgentCard {
        id: scout(),
        name: "Scout".into(),
        monogram: "Sc".into(),
    }]);
    (cards, tracker)
}

fn shown(views: &(RunCards, CorralTracker)) -> (CardStatus, Vec<CardAction>) {
    let card = views.0.card(&scout(), 2_000).expect("card");
    (card.status, card.actions)
}

fn drop_state(views: &(RunCards, CorralTracker)) -> DropState {
    views.1.view().drops[0].state
}

// ---- TP-PAUSE-01: the card's Pause, Pausing, Paused and Resume -----------------------------

#[test]
fn tp_pause_01_only_an_agent_that_can_pause_offers_pause() {
    let mut cannot = views(false);
    working(&mut cannot, "r1");
    assert_eq!(
        shown(&cannot),
        (CardStatus::Working, vec![CardAction::Stop])
    );
    assert!(!cannot.0.ask_pause(&scout(), true), "nothing to ask");
    assert_eq!(shown(&cannot).0, CardStatus::Working);

    let mut can = views(true);
    working(&mut can, "r1");
    assert_eq!(
        shown(&can),
        (
            CardStatus::Working,
            vec![CardAction::Pause, CardAction::Stop]
        )
    );
}

#[test]
fn tp_pause_01_pause_reads_pausing_then_paused_and_resume_carries_on() {
    let mut v = views(true);
    working(&mut v, "r1");
    assert!(v.0.ask_pause(&scout(), true), "the card changed");
    assert_eq!(
        shown(&v),
        (
            CardStatus::Pausing,
            vec![CardAction::Resume, CardAction::Stop]
        )
    );
    assert!(
        !v.0.ask_pause(&scout(), true),
        "asking again changes nothing"
    );
    paused(&mut v, "r1");
    assert_eq!(
        shown(&v),
        (
            CardStatus::Paused,
            vec![CardAction::Resume, CardAction::Stop]
        )
    );
    resumed(&mut v, "r1");
    assert_eq!(
        shown(&v),
        (
            CardStatus::Working,
            vec![CardAction::Pause, CardAction::Stop]
        ),
        "paused once: the ask was used up"
    );
}

#[test]
fn tp_pause_01_resume_before_the_safe_point_takes_the_pause_back() {
    let mut v = views(true);
    working(&mut v, "r1");
    v.0.ask_pause(&scout(), true);
    assert!(v.0.ask_pause(&scout(), false));
    assert_eq!(
        shown(&v),
        (
            CardStatus::Working,
            vec![CardAction::Pause, CardAction::Stop]
        )
    );
    assert!(!v.0.ask_pause(&scout(), false), "nothing to take back");
}

#[test]
fn tp_pause_01_only_a_working_run_can_be_asked_and_the_ask_ends_with_it() {
    let mut v = views(true);
    see(
        &mut v,
        "r1",
        EventBody::RunQueued(RunQueued {
            agent_id: scout(),
            session_id: SessionId::from("s-r1"),
            prompt: "Tidy my notes.".into(),
        }),
    );
    assert_eq!(shown(&v), (CardStatus::Queued, vec![CardAction::Stop]));
    assert!(!v.0.ask_pause(&scout(), true), "a queued run can't pause");
    see(
        &mut v,
        "r1",
        EventBody::RunStarted(RunStarted {
            adapter: "fake".into(),
        }),
    );
    see(
        &mut v,
        "r1",
        EventBody::CheckpointOpened(CheckpointOpened {
            checkpoint_id: CheckpointId::from("cp-1"),
            tool_call_id: ToolCallId::from("call-1"),
            action_class: ActionClass::WriteLocal,
            risk: Risk::Normal,
            title: "Saving a file".into(),
            detail: "notes.md".into(),
        }),
    );
    assert!(
        !v.0.ask_pause(&scout(), true),
        "a run waiting on you shows Approve and Deny, not Pause"
    );
    see(
        &mut v,
        "r1",
        EventBody::CheckpointResolved(CheckpointResolved {
            checkpoint_id: CheckpointId::from("cp-1"),
            resolution: Resolution::Approved,
        }),
    );
    assert!(v.0.ask_pause(&scout(), true));
    see(
        &mut v,
        "r1",
        EventBody::RunCancelled(RunCancelled {
            by: CancelledBy::User,
        }),
    );
    assert_eq!(
        shown(&v).0,
        CardStatus::Stopped,
        "the ask ended with the run"
    );
    working(&mut v, "r2");
    assert_eq!(
        shown(&v),
        (
            CardStatus::Working,
            vec![CardAction::Pause, CardAction::Stop]
        ),
        "the next run starts unasked"
    );
}

// ---- TP-PAUSE-02: a paused drop has its own state ------------------------------------------

#[test]
fn tp_pause_02_a_paused_drop_reads_paused_and_back_to_working() {
    let mut v = views(true);
    working(&mut v, "r1");
    v.0.ask_pause(&scout(), true);
    assert_eq!(
        drop_state(&v),
        DropState::Working,
        "still working until its safe point"
    );
    paused(&mut v, "r1");
    assert_eq!(drop_state(&v), DropState::Paused);
    resumed(&mut v, "r1");
    assert_eq!(drop_state(&v), DropState::Working);
}

#[test]
fn tp_pause_02_a_run_working_or_waiting_on_you_outranks_a_paused_one() {
    let mut v = views(true);
    working(&mut v, "r1");
    paused(&mut v, "r1");
    working(&mut v, "r2");
    assert_eq!(drop_state(&v), DropState::Working);
    see(
        &mut v,
        "r2",
        EventBody::CheckpointOpened(CheckpointOpened {
            checkpoint_id: CheckpointId::from("cp-1"),
            tool_call_id: ToolCallId::from("call-1"),
            action_class: ActionClass::WriteLocal,
            risk: Risk::Normal,
            title: "Saving a file".into(),
            detail: "notes.md".into(),
        }),
    );
    assert_eq!(drop_state(&v), DropState::NeedsYou);
}

// ---- TP-PAUSE-03: the controls -------------------------------------------------------------

#[derive(Default)]
struct MemLog(Mutex<Vec<Event>>);

impl EventLog for MemLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        self.0.lock().expect("log").push(event.clone());
        Ok(())
    }
}

/// A slow step with a safe point before each; pauses only if `pauses`.
struct Slow {
    pauses: bool,
}

impl RunDriver for Slow {
    fn run(&self, _req: RunRequest, sink: EventSink, cancel: CancelToken) -> RunFuture {
        Box::pin(async move {
            for _ in 0..3 {
                if !sink.pause_point(&cancel).await {
                    return;
                }
                tokio::select! {
                    () = sleep(Duration::from_secs(1)) => {}
                    () = cancel.cancelled() => return,
                }
            }
            sink.emit(EventBody::RunCompleted(RunCompleted { summary: None }))
                .expect("completed");
        })
    }

    fn pauses(&self, _agent: &AgentId) -> bool {
        self.pauses
    }
}

fn controls(pauses: bool) -> (CorralControls, Supervisor) {
    let sup = Supervisor::new(
        SupervisorConfig::default(),
        Arc::new(Slow { pauses }),
        Arc::new(MemLog::default()),
        Arc::new(ManualClock::at(1_000)),
    );
    let n = AtomicU64::new(1);
    let controls = CorralControls::new(
        sup.clone(),
        [scout()],
        Arc::new(move || n.fetch_add(1, Ordering::SeqCst).to_string()),
    );
    (controls, sup)
}

#[tokio::test(start_paused = true)]
async fn tp_pause_03_pause_and_resume_reach_the_supervisor() {
    let (controls, sup) = controls(true);
    assert!(sup.can_pause(&scout()));
    let run = controls.deploy(&scout(), "Tidy my notes.").expect("deploy");
    sleep(Duration::from_millis(500)).await;
    assert_eq!(controls.pause(&scout()), Ok(run.clone()));
    sleep(Duration::from_secs(5)).await;
    assert_eq!(sup.phase(&run), Some(Phase::Paused));
    assert_eq!(controls.resume(&scout()), Ok(run.clone()));
    sleep(Duration::from_millis(10)).await;
    assert_eq!(sup.phase(&run), Some(Phase::Running));
}

#[tokio::test(start_paused = true)]
async fn tp_pause_03_an_agent_that_cant_pause_or_isnt_working_is_refused() {
    let (controls, sup) = controls(false);
    assert!(!sup.can_pause(&scout()));
    assert_eq!(
        controls.pause(&scout()),
        Err(ControlError::NothingToPause(scout())),
        "nothing working"
    );
    controls.deploy(&scout(), "Tidy my notes.").expect("deploy");
    sleep(Duration::from_millis(500)).await;
    assert_eq!(
        controls.pause(&scout()),
        Err(ControlError::CantPause(scout()))
    );
    let nobody = AgentId::from("nobody");
    assert_eq!(
        controls.pause(&nobody),
        Err(ControlError::UnknownAgent(nobody.clone()))
    );
    assert_eq!(
        controls.resume(&nobody),
        Err(ControlError::UnknownAgent(nobody))
    );
}

#[tokio::test(start_paused = true)]
async fn tp_pause_03_a_finished_run_has_nothing_to_pause_or_resume() {
    let (controls, _sup) = controls(true);
    controls.deploy(&scout(), "Tidy my notes.").expect("deploy");
    sleep(Duration::from_secs(10)).await;
    assert_eq!(
        controls.pause(&scout()),
        Err(ControlError::NothingToPause(scout()))
    );
    assert_eq!(
        controls.resume(&scout()),
        Err(ControlError::NothingToPause(scout()))
    );
}
