//! Run lifecycle: TP-LIFE-01..04 (`docs/04-test-plan.md`, system design §3 "Run lifecycle").

use corral_core::domain::{ActionClass, Risk};
use corral_core::event::*;
use corral_core::ids::{AgentId, CheckpointId, RunId, SessionId, SpanId, ToolCallId};
use corral_core::lifecycle::{Lifecycle, LifecycleError, Phase, next};
use proptest::prelude::*;

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

fn queued(run: &str, session: &str) -> Event {
    ev(
        run,
        EventBody::RunQueued(RunQueued {
            agent_id: AgentId::from("grok"),
            session_id: SessionId::from(session),
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

fn resolved(run: &str, cp: &str, resolution: Resolution) -> Event {
    ev(
        run,
        EventBody::CheckpointResolved(CheckpointResolved {
            checkpoint_id: CheckpointId::from(cp),
            resolution,
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

fn resumed(run: &str) -> Event {
    ev(run, EventBody::RunResumed(RunResumed {}))
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
            by: CancelledBy::RecallAll,
        }),
    )
}

fn closed(run: &str) -> Event {
    ev(run, EventBody::SessionClosed(SessionClosed {}))
}

/// Applies events from Idle, failing the test on the first rejected one.
fn drive(events: &[Event]) -> Lifecycle {
    events.iter().fold(Lifecycle::idle(), |state, e| {
        next(&state, e).unwrap_or_else(|err| panic!("{} rejected: {err}", e.kind().as_str()))
    })
}

fn running(run: &str) -> Vec<Event> {
    vec![queued(run, "s1"), started(run)]
}

fn with(mut base: Vec<Event>, more: &[Event]) -> Vec<Event> {
    base.extend_from_slice(more);
    base
}

// ---- TP-LIFE-01 ---------------------------------------------------------------------------

#[test]
fn tp_life_01_every_legal_transition_in_the_diagram_is_accepted() {
    // (events from Idle, expected phase) for every arrow in the diagram.
    let cases: Vec<(Vec<Event>, Phase)> = vec![
        (vec![queued("r1", "s1")], Phase::Queued),
        (running("r1"), Phase::Running),
        (with(running("r1"), &[delta("r1")]), Phase::Running),
        (
            with(running("r1"), &[opened("r1", "c1")]),
            Phase::AwaitingCheckpoint,
        ),
        (
            with(
                running("r1"),
                &[
                    opened("r1", "c1"),
                    resolved("r1", "c1", Resolution::Approved),
                ],
            ),
            Phase::Running,
        ),
        (
            with(
                running("r1"),
                &[
                    opened("r1", "c1"),
                    resolved("r1", "c1", Resolution::Denied { note: None }),
                ],
            ),
            Phase::Running,
        ),
        (
            with(
                running("r1"),
                &[
                    opened("r1", "c1"),
                    resolved("r1", "c1", Resolution::Expired),
                    cancelled("r1"),
                ],
            ),
            Phase::Cancelled,
        ),
        (with(running("r1"), &[paused("r1")]), Phase::Paused),
        (
            with(running("r1"), &[paused("r1"), resumed("r1")]),
            Phase::Running,
        ),
        (with(running("r1"), &[completed("r1")]), Phase::Completed),
        (with(running("r1"), &[failed("r1")]), Phase::Failed),
        (with(running("r1"), &[cancelled("r1")]), Phase::Cancelled),
        (
            with(running("r1"), &[completed("r1"), closed("r1")]),
            Phase::Idle,
        ),
        (
            with(running("r1"), &[failed("r1"), closed("r1")]),
            Phase::Idle,
        ),
        (
            with(running("r1"), &[cancelled("r1"), closed("r1")]),
            Phase::Idle,
        ),
    ];
    for (events, expected) in cases {
        let kinds: Vec<&str> = events.iter().map(|e| e.kind().as_str()).collect();
        assert_eq!(drive(&events).phase(), expected, "after {kinds:?}");
    }
}

#[test]
fn tp_life_01_retry_recall_all_and_several_checkpoints() {
    // Retry from an error (07 §8.2): a new run in the same session.
    let retried = drive(&with(running("r1"), &[failed("r1"), queued("r2", "s1")]));
    assert_eq!(retried.phase(), Phase::Queued);

    // Recall-all stops every working agent, not only running ones.
    for prefix in [
        vec![queued("r1", "s1")],
        with(running("r1"), &[paused("r1")]),
        with(running("r1"), &[opened("r1", "c1")]),
    ] {
        assert_eq!(
            drive(&with(prefix, &[cancelled("r1")])).phase(),
            Phase::Cancelled
        );
    }

    // Several checkpoints can be pending ("1 of 3"); the run resumes when the last is answered.
    let two_open = drive(&with(
        running("r1"),
        &[opened("r1", "c1"), opened("r1", "c2")],
    ));
    assert_eq!(two_open.open_checkpoints().len(), 2);
    let one_left =
        next(&two_open, &resolved("r1", "c1", Resolution::AllowedForRun)).expect("resolve c1");
    assert_eq!(one_left.phase(), Phase::AwaitingCheckpoint);
    let none_left =
        next(&one_left, &resolved("r1", "c2", Resolution::Approved)).expect("resolve c2");
    assert_eq!(none_left.phase(), Phase::Running);
    assert!(none_left.open_checkpoints().is_empty());
}

#[test]
fn tp_life_01_illegal_transitions_are_errors_not_no_ops() {
    let idle = Lifecycle::idle();
    assert!(matches!(
        next(&idle, &started("r1")),
        Err(LifecycleError::IllegalTransition { .. })
    ));

    let done = drive(&with(running("r1"), &[completed("r1")]));
    assert!(matches!(
        next(&done, &delta("r1")),
        Err(LifecycleError::IllegalTransition { .. })
    ));

    let run = drive(&running("r1"));
    assert!(matches!(
        next(&run, &delta("someone-else")),
        Err(LifecycleError::WrongRun { .. })
    ));
    assert!(matches!(
        next(&run, &resolved("r1", "never-opened", Resolution::Approved)),
        Err(LifecycleError::IllegalTransition { .. })
    ));

    let waiting = drive(&with(running("r1"), &[opened("r1", "c1")]));
    assert!(matches!(
        next(&waiting, &resolved("r1", "c9", Resolution::Approved)),
        Err(LifecycleError::UnknownCheckpoint { .. })
    ));
    assert!(matches!(
        next(&waiting, &opened("r1", "c1")),
        Err(LifecycleError::DuplicateCheckpoint { .. })
    ));
}

// ---- TP-LIFE-03 / TP-LIFE-04 ----------------------------------------------------------------

#[test]
fn tp_life_03_prompt_again_is_a_new_run_in_the_same_session() {
    let done = drive(&with(running("r1"), &[completed("r1")]));
    let again = next(&done, &queued("r2", "s1")).expect("prompt again");

    assert_eq!(again.phase(), Phase::Queued);
    assert_eq!(again.session_id(), Some(&SessionId::from("s1")));
    assert_eq!(again.run_id(), Some(&RunId::from("r2")));
    assert_eq!(again.runs(), [RunId::from("r1"), RunId::from("r2")]);

    // A different session, or reusing the old run's ID, is refused.
    assert!(matches!(
        next(&done, &queued("r2", "s2")),
        Err(LifecycleError::SessionMismatch { .. })
    ));
    assert!(matches!(
        next(&done, &queued("r1", "s1")),
        Err(LifecycleError::DuplicateRun { .. })
    ));
}

#[test]
fn tp_life_04_send_back_closes_the_session_and_returns_to_idle() {
    let done = drive(&with(
        running("r1"),
        &[
            completed("r1"),
            queued("r2", "s1"),
            started("r2"),
            completed("r2"),
        ],
    ));
    let back = next(&done, &closed("r2")).expect("send back");

    assert_eq!(back.phase(), Phase::Idle);
    assert_eq!(back, Lifecycle::idle());

    // The next deploy starts a fresh session.
    let fresh = next(&back, &queued("r3", "s2")).expect("deploy again");
    assert_eq!(fresh.session_id(), Some(&SessionId::from("s2")));
    assert_eq!(fresh.runs(), [RunId::from("r3")]);
}

// ---- TP-LIFE-02 (property) ------------------------------------------------------------------

/// The diagram, written independently of the implementation: may `kind` follow `phase`?
/// Assumes the event is about the current run and session and its checkpoint IDs line up.
fn diagram_allows(phase: Phase, kind: EventKind, open_checkpoints: usize) -> bool {
    use Phase as P;
    let working = matches!(
        phase,
        P::Queued | P::Running | P::AwaitingCheckpoint | P::Paused
    );
    match kind {
        EventKind::RunQueued => matches!(phase, P::Idle | P::Completed | P::Failed),
        EventKind::RunStarted => phase == P::Queued,
        EventKind::PlanUpdated
        | EventKind::ModelRequest
        | EventKind::ModelDelta
        | EventKind::ModelResponse
        | EventKind::BudgetWarned => phase == P::Running,
        EventKind::ToolRequested
        | EventKind::PolicyDecided
        | EventKind::ToolStarted
        | EventKind::ToolResult
        | EventKind::UsageReported => matches!(phase, P::Running | P::AwaitingCheckpoint),
        EventKind::CheckpointOpened => matches!(phase, P::Running | P::AwaitingCheckpoint),
        EventKind::CheckpointResolved => phase == P::AwaitingCheckpoint && open_checkpoints > 0,
        EventKind::RunPaused => phase == P::Running,
        EventKind::RunResumed => phase == P::Paused,
        EventKind::RunCompleted => phase == P::Running,
        EventKind::RunFailed | EventKind::RunCancelled => working,
        EventKind::SessionClosed => matches!(phase, P::Completed | P::Failed | P::Cancelled),
    }
}

/// Invariants every reachable state must hold.
fn assert_well_formed(s: &Lifecycle) {
    match s.phase() {
        Phase::Idle => {
            assert!(s.session_id().is_none() && s.run_id().is_none() && s.runs().is_empty());
            assert!(s.open_checkpoints().is_empty());
        }
        Phase::Queued
        | Phase::Running
        | Phase::AwaitingCheckpoint
        | Phase::Paused
        | Phase::Completed
        | Phase::Failed
        | Phase::Cancelled => {
            let run = s.run_id().expect("a non-idle state has a run");
            assert!(s.session_id().is_some());
            assert_eq!(s.runs().last(), Some(run), "the current run is the latest");
        }
    }
    assert_eq!(
        s.phase() == Phase::AwaitingCheckpoint,
        !s.open_checkpoints().is_empty(),
        "waiting exactly when a checkpoint is open"
    );
}

/// One step: a kind, and whether the event is about the current run or a stranger.
fn step() -> impl Strategy<Value = (usize, bool)> {
    (0..EventKind::ALL.len(), prop::bool::weighted(0.9))
}

/// Builds an event of `kind` for the current run (or a stranger), with IDs that line up.
fn event_for(state: &Lifecycle, kind: EventKind, current: bool, counter: usize) -> Event {
    let run = match (current, state.run_id()) {
        (true, Some(r)) => r.as_str().to_owned(),
        (true, None) | (false, _) => format!("stranger-{counter}"),
    };
    let next_run = format!("run-{counter}");
    let session = state
        .session_id()
        .map_or_else(|| "s-new".to_owned(), |s| s.as_str().to_owned());
    let cp_open = state
        .open_checkpoints()
        .first()
        .map(|c| c.as_str().to_owned());
    match kind {
        EventKind::RunQueued => queued(&next_run, &session),
        EventKind::RunStarted => started(&run),
        EventKind::PlanUpdated => ev(&run, EventBody::PlanUpdated(PlanUpdated { steps: vec![] })),
        EventKind::ModelRequest => ev(
            &run,
            EventBody::ModelRequest(ModelRequest {
                provider: "fake".into(),
                model: "fake".into(),
            }),
        ),
        EventKind::ModelDelta => delta(&run),
        EventKind::ModelResponse => ev(
            &run,
            EventBody::ModelResponse(ModelResponse {
                text: String::new(),
                finish_reason: None,
            }),
        ),
        EventKind::ToolRequested => ev(
            &run,
            EventBody::ToolRequested(ToolRequested {
                tool_call_id: ToolCallId::from("call"),
                tool: "t".into(),
                action_class: ActionClass::ReadLocal,
                args: serde_json::Value::Null,
            }),
        ),
        EventKind::PolicyDecided => ev(
            &run,
            EventBody::PolicyDecided(PolicyDecided {
                tool_call_id: ToolCallId::from("call"),
                decision: PolicyDecision::Allow,
            }),
        ),
        EventKind::CheckpointOpened => opened(&run, &format!("cp-{counter}")),
        EventKind::CheckpointResolved => resolved(
            &run,
            &cp_open.unwrap_or_else(|| "cp-none".to_owned()),
            Resolution::Approved,
        ),
        EventKind::ToolStarted => ev(
            &run,
            EventBody::ToolStarted(ToolStarted {
                tool_call_id: ToolCallId::from("call"),
            }),
        ),
        EventKind::ToolResult => ev(
            &run,
            EventBody::ToolResult(ToolResult {
                tool_call_id: ToolCallId::from("call"),
                ok: true,
                output: serde_json::Value::Null,
            }),
        ),
        EventKind::UsageReported => ev(
            &run,
            EventBody::UsageReported(UsageReported {
                input_tokens: 1,
                output_tokens: 1,
                cost_micro_usd: 1,
            }),
        ),
        EventKind::BudgetWarned => ev(
            &run,
            EventBody::BudgetWarned(BudgetWarned {
                scope: BudgetScope::Run,
                spent_micro_usd: 1,
                limit_micro_usd: 2,
            }),
        ),
        EventKind::RunPaused => paused(&run),
        EventKind::RunResumed => resumed(&run),
        EventKind::RunCompleted => completed(&run),
        EventKind::RunFailed => failed(&run),
        EventKind::RunCancelled => cancelled(&run),
        EventKind::SessionClosed => closed(&run),
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn tp_life_02_random_sequences_follow_the_diagram(steps in prop::collection::vec(step(), 1..60)) {
        let mut state = Lifecycle::idle();
        for (i, (k, current)) in steps.into_iter().enumerate() {
            let kind = EventKind::ALL[k];
            let event = event_for(&state, kind, current, i);
            let about_current_run = kind == EventKind::RunQueued
                || state.run_id().is_some_and(|r| *r == event.run_id);
            let expected = about_current_run
                && diagram_allows(state.phase(), kind, state.open_checkpoints().len());

            match next(&state, &event) {
                Ok(new_state) => {
                    prop_assert!(expected, "{:?} accepted {} but the diagram forbids it", state.phase(), kind.as_str());
                    assert_well_formed(&new_state);
                    state = new_state;
                }
                Err(_) => {
                    prop_assert!(!expected, "{:?} rejected {} but the diagram allows it", state.phase(), kind.as_str());
                }
            }
        }
    }
}
