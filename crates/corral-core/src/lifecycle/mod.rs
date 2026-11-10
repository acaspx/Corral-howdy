//! The run lifecycle: a pure function from (state, event) to the next state (system design §3).
//!
//! The state is an agent's: idle in the corral, or working on one run of one session. "Prompt
//! again" queues a new run in the same session; "Send back" closes the session and returns the
//! agent to [`Phase::Idle`]. Illegal transitions are errors, never silent no-ops.
//!
//! Beyond the arrows in the diagram, the machine follows the rest of the spec:
//! - **Retry** after an error queues a new run in the same session (07 §8.2).
//! - **Recall-all** stops every working agent, so a queued, waiting or paused run can be
//!   cancelled, and can fail, as well as a running one (01 §8).
//! - **Several checkpoints** can be pending at once ("1 of 3", 07 §8.2); the run goes back to
//!   running when the last one is answered. Tool traffic for other, allowed calls may continue
//!   while it waits.

use crate::event::{Event, EventBody, EventKind, RunQueued};
use crate::ids::{CheckpointId, RunId, SessionId};

/// Where an agent's work stands. Names follow the diagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Phase {
    /// In the corral, no session.
    #[default]
    Idle,
    /// A deploy was accepted; the adapter hasn't started.
    Queued,
    /// Working.
    Running,
    /// Waiting on one or more checkpoints.
    AwaitingCheckpoint,
    /// Paused by the user or a budget soft limit.
    Paused,
    /// Finished; the session is still open for a follow-up.
    Completed,
    /// Ended with an error; Retry or Send back.
    Failed,
    /// Stopped by the user, Recall-all or an expired checkpoint.
    Cancelled,
}

/// Why an event can't follow the current state.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LifecycleError {
    /// The diagram has no arrow for this event from this phase.
    #[error("`{kind}` can't follow {from:?}", kind = kind.as_str())]
    IllegalTransition {
        /// The current phase.
        from: Phase,
        /// The event that was refused.
        kind: EventKind,
    },
    /// The event is about a different run than the current one.
    #[error("event is for run {got}, but the current run is {expected}")]
    WrongRun {
        /// The current run.
        expected: RunId,
        /// The event's run.
        got: RunId,
    },
    /// A follow-up was queued in a different session.
    #[error("follow-up is for session {got}, but the open session is {expected}")]
    SessionMismatch {
        /// The open session.
        expected: SessionId,
        /// The event's session.
        got: SessionId,
    },
    /// A follow-up reused a run ID from this session.
    #[error("run {0} already exists in this session")]
    DuplicateRun(RunId),
    /// A checkpoint was opened twice.
    #[error("checkpoint {0} is already open")]
    DuplicateCheckpoint(CheckpointId),
    /// A checkpoint that isn't open was resolved.
    #[error("checkpoint {0} isn't open")]
    UnknownCheckpoint(CheckpointId),
}

/// An agent's lifecycle state. Build it with [`Lifecycle::idle`] and advance it with [`next`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Lifecycle {
    phase: Phase,
    session_id: Option<SessionId>,
    runs: Vec<RunId>,
    open_checkpoints: Vec<CheckpointId>,
}

impl Lifecycle {
    /// An agent in the corral with no session.
    pub fn idle() -> Self {
        Self::default()
    }

    /// The current phase.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The open session, if any.
    pub fn session_id(&self) -> Option<&SessionId> {
        self.session_id.as_ref()
    }

    /// The current (latest) run, if any.
    pub fn run_id(&self) -> Option<&RunId> {
        self.runs.last()
    }

    /// Every run in the open session, oldest first. Empty when idle.
    pub fn runs(&self) -> &[RunId] {
        &self.runs
    }

    /// Checkpoints waiting on the user, in the order they opened.
    pub fn open_checkpoints(&self) -> &[CheckpointId] {
        &self.open_checkpoints
    }

    fn with_phase(&self, phase: Phase) -> Self {
        Self {
            phase,
            ..self.clone()
        }
    }
}

/// The next state after `event`, or why `event` can't happen now. Pure: no I/O, no clock.
pub fn next(state: &Lifecycle, event: &Event) -> Result<Lifecycle, LifecycleError> {
    use Phase as P;

    let from = state.phase;
    let kind = event.kind();
    let illegal = || LifecycleError::IllegalTransition { from, kind };

    // Every event but a deploy is about the current run.
    let check = |allowed: &[Phase]| -> Result<(), LifecycleError> {
        if !allowed.contains(&from) {
            return Err(illegal());
        }
        match state.run_id() {
            Some(current) if *current == event.run_id => Ok(()),
            Some(current) => Err(LifecycleError::WrongRun {
                expected: current.clone(),
                got: event.run_id.clone(),
            }),
            None => Err(illegal()),
        }
    };
    const WORKING: [Phase; 4] = [P::Queued, P::Running, P::AwaitingCheckpoint, P::Paused];

    match &event.body {
        EventBody::RunQueued(queued) => queue(state, event, queued),
        EventBody::RunStarted(_) => {
            check(&[P::Queued])?;
            Ok(state.with_phase(P::Running))
        }
        EventBody::PlanUpdated(_)
        | EventBody::ModelRequest(_)
        | EventBody::ModelDelta(_)
        | EventBody::ModelResponse(_)
        | EventBody::BudgetWarned(_) => {
            check(&[P::Running])?;
            Ok(state.clone())
        }
        // Other, allowed tool calls may finish while a checkpoint waits.
        EventBody::ToolRequested(_)
        | EventBody::PolicyDecided(_)
        | EventBody::ToolStarted(_)
        | EventBody::ToolResult(_)
        | EventBody::UsageReported(_) => {
            check(&[P::Running, P::AwaitingCheckpoint])?;
            Ok(state.clone())
        }
        EventBody::CheckpointOpened(opened) => {
            check(&[P::Running, P::AwaitingCheckpoint])?;
            if state.open_checkpoints.contains(&opened.checkpoint_id) {
                return Err(LifecycleError::DuplicateCheckpoint(
                    opened.checkpoint_id.clone(),
                ));
            }
            let mut out = state.with_phase(P::AwaitingCheckpoint);
            out.open_checkpoints.push(opened.checkpoint_id.clone());
            Ok(out)
        }
        EventBody::CheckpointResolved(resolved) => {
            check(&[P::AwaitingCheckpoint])?;
            let mut out = state.clone();
            let before = out.open_checkpoints.len();
            out.open_checkpoints
                .retain(|c| *c != resolved.checkpoint_id);
            if out.open_checkpoints.len() == before {
                return Err(LifecycleError::UnknownCheckpoint(
                    resolved.checkpoint_id.clone(),
                ));
            }
            if out.open_checkpoints.is_empty() {
                out.phase = P::Running;
            }
            Ok(out)
        }
        EventBody::RunPaused(_) => {
            check(&[P::Running])?;
            Ok(state.with_phase(P::Paused))
        }
        EventBody::RunResumed(_) => {
            check(&[P::Paused])?;
            Ok(state.with_phase(P::Running))
        }
        EventBody::RunCompleted(_) => {
            check(&[P::Running])?;
            Ok(state.with_phase(P::Completed))
        }
        EventBody::RunFailed(_) => {
            check(&WORKING)?;
            Ok(ended(state, P::Failed))
        }
        EventBody::RunCancelled(_) => {
            check(&WORKING)?;
            Ok(ended(state, P::Cancelled))
        }
        EventBody::SessionClosed(_) => {
            check(&[P::Completed, P::Failed, P::Cancelled])?;
            Ok(Lifecycle::idle())
        }
    }
}

/// A run ended early: no checkpoint can still be waiting.
fn ended(state: &Lifecycle, phase: Phase) -> Lifecycle {
    Lifecycle {
        phase,
        open_checkpoints: Vec::new(),
        ..state.clone()
    }
}

/// A deploy: the first run of a new session from Idle, or a follow-up ("prompt again", Retry)
/// in the open session after the last run completed or failed.
fn queue(
    state: &Lifecycle,
    event: &Event,
    queued: &RunQueued,
) -> Result<Lifecycle, LifecycleError> {
    let illegal = LifecycleError::IllegalTransition {
        from: state.phase,
        kind: EventKind::RunQueued,
    };
    match state.phase {
        Phase::Idle => Ok(Lifecycle {
            phase: Phase::Queued,
            session_id: Some(queued.session_id.clone()),
            runs: vec![event.run_id.clone()],
            open_checkpoints: Vec::new(),
        }),
        Phase::Completed | Phase::Failed => {
            match &state.session_id {
                Some(open) if *open == queued.session_id => {}
                Some(open) => {
                    return Err(LifecycleError::SessionMismatch {
                        expected: open.clone(),
                        got: queued.session_id.clone(),
                    });
                }
                None => return Err(illegal),
            }
            if state.runs.contains(&event.run_id) {
                return Err(LifecycleError::DuplicateRun(event.run_id.clone()));
            }
            let mut out = state.with_phase(Phase::Queued);
            out.runs.push(event.run_id.clone());
            Ok(out)
        }
        Phase::Queued
        | Phase::Running
        | Phase::AwaitingCheckpoint
        | Phase::Paused
        | Phase::Cancelled => Err(illegal),
    }
}
