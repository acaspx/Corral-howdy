//! The supervisor: runs agents, one task per run, under a global concurrency limit, and stops them
//! on Stop or Recall-all (system design §8).
//!
//! - **Deploy** logs `run.queued`. If fewer than `max_concurrent` runs are active, the run starts
//!   (`run.started`) and its [`RunDriver`] runs on its own task; otherwise it waits in a FIFO queue.
//! - **Every event** a driver emits goes through [`EventSink`], which assigns `seq` and `ts`,
//!   checks it against the run lifecycle ([`crate::lifecycle::next`]) and only then appends it to
//!   the [`EventLog`]. An illegal event is refused and never reaches the log.
//! - **A driver that returns without finishing** fails the run (`FailureReason::Adapter`), so no
//!   run is left hanging.
//! - **Stop and Recall-all** fire the run's [`CancelToken`]. A driver that hasn't stopped after
//!   `force_drop_after` (2 s by default) is dropped: its future is aborted, which closes HTTP
//!   streams and runs the adapter's cleanup. Either way the run ends with `run.cancelled`.
//!
//! The supervisor never decides policy or budgets; those arrive with M2. Runs here are
//! independent: follow-up runs in a session ("prompt again") are wired up with the session
//! layer.
//!
//! The [`EventLog`] is called from async tasks. The SQLite store is fast enough for M1; if it
//! ever blocks noticeably, the call moves to `spawn_blocking`.

use crate::clock::Clock;
use crate::event::{
    CancelledBy, Event, EventBody, FailureReason, RunCancelled, RunFailed, RunQueued, RunStarted,
};
use crate::ids::{AgentId, RunId, SessionId, SpanId};
use crate::lifecycle::{Lifecycle, LifecycleError, Phase, next};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;

/// A driver's work for one run.
pub type RunFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Where events go: the store in the app, an in-memory log in tests.
pub trait EventLog: Send + Sync + 'static {
    /// Appends one event. Called in `seq` order for each run.
    fn append(&self, event: &Event) -> Result<(), LogError>;
}

/// An [`EventLog`] that tells an observer about each event after the inner log accepts it, so
/// projections such as the corral view (SK-2b) follow the log exactly. An event the inner log
/// refuses reaches no observer. The observer runs on the appending task, in `seq` order for each
/// run, and must return quickly: hand slow work elsewhere.
pub struct WatchedLog<F> {
    inner: Arc<dyn EventLog>,
    observer: F,
}

impl<F: Fn(&Event) + Send + Sync + 'static> WatchedLog<F> {
    /// Wraps `inner`; `observer` sees every event it accepts.
    pub fn new(inner: Arc<dyn EventLog>, observer: F) -> Self {
        Self { inner, observer }
    }
}

impl<F: Fn(&Event) + Send + Sync + 'static> EventLog for WatchedLog<F> {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        self.inner.append(event)?;
        (self.observer)(event);
        Ok(())
    }
}

/// An [`EventLog`] failure, with the log's own message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("event log: {0}")]
pub struct LogError(pub String);

/// Runs one agent run. Implemented by every adapter (system design §4); the supervisor doesn't
/// know which provider is behind it.
pub trait RunDriver: Send + Sync + 'static {
    /// Starts the run. Emit events through `sink`, finish with `run.completed` or `run.failed`,
    /// and return promptly once `cancel` fires.
    fn run(&self, request: RunRequest, sink: EventSink, cancel: CancelToken) -> RunFuture;
}

/// What to deploy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequest {
    /// The new run's ID (unique).
    pub run_id: RunId,
    /// The agent to run.
    pub agent_id: AgentId,
    /// The session it belongs to.
    pub session_id: SessionId,
    /// The task.
    pub prompt: String,
}

/// Limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorConfig {
    /// Runs allowed at once; more wait in a FIFO queue (system design §8: default 8).
    pub max_concurrent: usize,
    /// How long a cancelled driver gets to stop before it's dropped (default 2 s).
    pub force_drop_after: Duration,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 8,
            force_drop_after: Duration::from_secs(2),
        }
    }
}

/// Why the supervisor refused something.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SupervisorError {
    /// The event can't follow the run's current state.
    #[error(transparent)]
    Lifecycle(#[from] LifecycleError),
    /// The log refused the event.
    #[error(transparent)]
    Log(#[from] LogError),
    /// A run with this ID was already deployed.
    #[error("run {0} was already deployed")]
    DuplicateRun(RunId),
    /// No active or queued run has this ID.
    #[error("no active or queued run {0}")]
    UnknownRun(RunId),
    /// Internal state was poisoned by a panic.
    #[error("the supervisor's state is poisoned")]
    Poisoned,
}

/// Fires once to ask a driver to stop.
#[derive(Debug, Clone)]
pub struct CancelToken(watch::Receiver<bool>);

impl CancelToken {
    /// True once cancel has fired.
    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolves when cancel fires (or the supervisor goes away).
    pub async fn cancelled(&self) {
        let mut rx = self.0.clone();
        // An error means the sender is gone, which also means "stop".
        let _ = rx.wait_for(|cancelled| *cancelled).await;
    }
}

/// What Recall-all did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecallReport {
    /// Runs stopped, running and queued.
    pub stopped: usize,
    /// Of those, drivers that ignored cancel and were dropped.
    pub forced: usize,
}

/// One run's log state: its lifecycle and next seq. Shared by its sink and the supervisor.
#[derive(Debug)]
struct RunLog {
    run_id: RunId,
    span_id: SpanId,
    state: Lifecycle,
    next_seq: u64,
}

/// Emits a run's events: assigns `seq` and `ts`, enforces the lifecycle, appends to the log.
#[derive(Clone)]
pub struct EventSink {
    run: Arc<Mutex<RunLog>>,
    log: Arc<dyn EventLog>,
    clock: Arc<dyn Clock>,
}

impl EventSink {
    /// Emits `body` as the run's next event. Refused (and not logged) if the lifecycle doesn't
    /// allow it now.
    pub fn emit(&self, body: EventBody) -> Result<(), SupervisorError> {
        let mut run = self.run.lock().map_err(|_| SupervisorError::Poisoned)?;
        let event = Event {
            run_id: run.run_id.clone(),
            seq: run.next_seq,
            ts: self.clock.now_ms(),
            span_id: run.span_id.clone(),
            parent_span_id: None,
            body,
        };
        let state = next(&run.state, &event)?;
        self.log.append(&event)?;
        run.state = state;
        run.next_seq += 1;
        Ok(())
    }

    fn phase(&self) -> Result<Phase, SupervisorError> {
        Ok(self
            .run
            .lock()
            .map_err(|_| SupervisorError::Poisoned)?
            .state
            .phase())
    }

    fn is_finished(&self) -> Result<bool, SupervisorError> {
        Ok(matches!(
            self.phase()?,
            Phase::Completed | Phase::Failed | Phase::Cancelled | Phase::Idle
        ))
    }
}

struct Active {
    sink: EventSink,
    cancel: watch::Sender<bool>,
    task: JoinHandle<()>,
    cancelled_by: Option<CancelledBy>,
}

struct Queued {
    request: RunRequest,
    sink: EventSink,
}

#[derive(Default)]
struct State {
    active: HashMap<RunId, Active>,
    queue: VecDeque<Queued>,
    /// Every run ever deployed, with its sink, for status queries and duplicate checks.
    known: HashMap<RunId, EventSink>,
}

struct Inner {
    config: SupervisorConfig,
    driver: Arc<dyn RunDriver>,
    log: Arc<dyn EventLog>,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
    idle: Notify,
}

/// Runs agents. Cheap to clone; all clones share one registry. Must be used inside a tokio
/// runtime.
#[derive(Clone)]
pub struct Supervisor(Arc<Inner>);

impl Supervisor {
    /// A supervisor that runs `driver` and logs to `log`, with time from `clock`.
    pub fn new(
        config: SupervisorConfig,
        driver: Arc<dyn RunDriver>,
        log: Arc<dyn EventLog>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self(Arc::new(Inner {
            config,
            driver,
            log,
            clock,
            state: Mutex::new(State::default()),
            idle: Notify::new(),
        }))
    }

    /// Logs `run.queued`, then starts the run now or queues it behind the concurrency limit.
    pub fn deploy(&self, request: RunRequest) -> Result<(), SupervisorError> {
        let mut state = self.lock()?;
        if state.known.contains_key(&request.run_id) {
            return Err(SupervisorError::DuplicateRun(request.run_id));
        }
        let sink = EventSink {
            run: Arc::new(Mutex::new(RunLog {
                run_id: request.run_id.clone(),
                span_id: SpanId::from(format!("{}:invoke_agent", request.run_id).as_str()),
                state: Lifecycle::idle(),
                next_seq: 1,
            })),
            log: self.0.log.clone(),
            clock: self.0.clock.clone(),
        };
        sink.emit(EventBody::RunQueued(RunQueued {
            agent_id: request.agent_id.clone(),
            session_id: request.session_id.clone(),
            prompt: request.prompt.clone(),
        }))?;
        state.known.insert(request.run_id.clone(), sink.clone());
        state.queue.push_back(Queued { request, sink });
        self.start_queued(&mut state)
    }

    /// Asks one run to stop (the hover card's Stop). Queued runs are cancelled at once; running
    /// ones get `force_drop_after` to stop before they're dropped.
    pub async fn cancel(&self, run_id: &RunId) -> Result<(), SupervisorError> {
        let pending = {
            let mut state = self.lock()?;
            if let Some(i) = state.queue.iter().position(|q| q.request.run_id == *run_id) {
                if let Some(q) = state.queue.remove(i) {
                    cancel_logged(&q.sink, CancelledBy::User)?;
                }
                return Ok(());
            }
            let active = state
                .active
                .get_mut(run_id)
                .ok_or_else(|| SupervisorError::UnknownRun(run_id.clone()))?;
            active.cancelled_by = Some(CancelledBy::User);
            active.cancel.send_replace(true);
            vec![run_id.clone()]
        };
        self.reap(pending).await.map(|_| ())
    }

    /// Recall-all: cancels every queued and running run, and drops drivers that don't stop
    /// within `force_drop_after`. Returns once every run has ended.
    pub async fn recall_all(&self) -> RecallReport {
        let (queued_stopped, running) = match self.lock() {
            Ok(mut state) => {
                let queued: Vec<Queued> = state.queue.drain(..).collect();
                let mut stopped = 0;
                for q in &queued {
                    if cancel_logged(&q.sink, CancelledBy::RecallAll).is_ok() {
                        stopped += 1;
                    }
                }
                let running: Vec<RunId> = state.active.keys().cloned().collect();
                for active in state.active.values_mut() {
                    active.cancelled_by = Some(CancelledBy::RecallAll);
                    active.cancel.send_replace(true);
                }
                (stopped, running)
            }
            Err(_) => (0, Vec::new()),
        };
        let stopped_running = running.len();
        let forced = self.reap(running).await.unwrap_or(0);
        RecallReport {
            stopped: queued_stopped + stopped_running,
            forced,
        }
    }

    /// Resolves when no run is active or queued.
    pub async fn wait_idle(&self) {
        loop {
            let notified = self.0.idle.notified();
            match self.lock() {
                Ok(state) if state.active.is_empty() && state.queue.is_empty() => return,
                Ok(_) => {}
                Err(_) => return,
            }
            notified.await;
        }
    }

    /// How many runs are running now.
    pub fn running(&self) -> usize {
        self.lock().map_or(0, |s| s.active.len())
    }

    /// Queued runs, oldest first.
    pub fn queued(&self) -> Vec<RunId> {
        self.lock().map_or_else(
            |_| Vec::new(),
            |s| s.queue.iter().map(|q| q.request.run_id.clone()).collect(),
        )
    }

    /// A deployed run's lifecycle phase.
    pub fn phase(&self, run_id: &RunId) -> Option<Phase> {
        let sink = self.lock().ok()?.known.get(run_id)?.clone();
        sink.phase().ok()
    }

    fn lock(&self) -> Result<MutexGuard<'_, State>, SupervisorError> {
        self.0.state.lock().map_err(|_| SupervisorError::Poisoned)
    }

    /// Starts queued runs while there's room. Caller holds the state lock.
    fn start_queued(&self, state: &mut State) -> Result<(), SupervisorError> {
        while state.active.len() < self.0.config.max_concurrent {
            let Some(Queued { request, sink }) = state.queue.pop_front() else {
                break;
            };
            sink.emit(EventBody::RunStarted(RunStarted {
                adapter: "driver".into(),
            }))?;
            let (cancel_tx, cancel_rx) = watch::channel(false);
            let run_id = request.run_id.clone();
            let work = self
                .0
                .driver
                .run(request, sink.clone(), CancelToken(cancel_rx));
            let me = self.clone();
            let finished_id = run_id.clone();
            let task = tokio::spawn(async move {
                work.await;
                me.finish(&finished_id);
            });
            state.active.insert(
                run_id,
                Active {
                    sink,
                    cancel: cancel_tx,
                    task,
                    cancelled_by: None,
                },
            );
        }
        if state.active.is_empty() && state.queue.is_empty() {
            self.0.idle.notify_waiters();
        }
        Ok(())
    }

    /// A driver returned: close out the run, free its slot, start the next queued run.
    fn finish(&self, run_id: &RunId) {
        let Ok(mut state) = self.lock() else { return };
        let Some(active) = state.active.remove(run_id) else {
            return;
        };
        close_out(&active.sink, active.cancelled_by);
        // A start failure here can only come from the log; the queued run stays queued and the
        // next finish tries again.
        let _ = self.start_queued(&mut state);
        self.0.idle.notify_waiters();
    }

    /// Waits for the given (already cancelled) runs to stop, dropping any still going after
    /// `force_drop_after`. Returns how many were dropped.
    async fn reap(&self, runs: Vec<RunId>) -> Result<usize, SupervisorError> {
        let deadline = tokio::time::Instant::now() + self.0.config.force_drop_after;
        let mut forced = 0;
        for run_id in runs {
            let task = {
                let mut state = self.lock()?;
                match state.active.get_mut(&run_id) {
                    // Swap in a finished placeholder so we can await the real handle unlocked.
                    Some(active) => std::mem::replace(&mut active.task, tokio::spawn(async {})),
                    None => continue, // already finished on its own
                }
            };
            let mut task = task;
            if tokio::time::timeout_at(deadline, &mut task).await.is_err() {
                task.abort();
                // Wait for the abort to land, so the driver's future (and with it any child
                // process or HTTP stream) is really gone before Recall-all reports done. The
                // result is always "cancelled"; there's nothing else to learn from it.
                let _ = task.await;
                forced += 1;
                // The driver's future is gone, so `finish` never ran for it: close it out here.
                let mut state = self.lock()?;
                if let Some(active) = state.active.remove(&run_id) {
                    close_out(&active.sink, active.cancelled_by);
                }
                self.start_queued(&mut state)?;
            }
        }
        Ok(forced)
    }
}

/// Ends a run whose driver is done: `run.cancelled` if it was cancelled, `run.failed` if it
/// returned without finishing, nothing if it already finished.
fn close_out(sink: &EventSink, cancelled_by: Option<CancelledBy>) {
    if sink.is_finished().unwrap_or(true) {
        return;
    }
    // The log is the only thing that can refuse these; the run is over either way.
    let _ = match cancelled_by {
        Some(by) => sink.emit(EventBody::RunCancelled(RunCancelled { by })),
        None => sink.emit(EventBody::RunFailed(RunFailed {
            reason: FailureReason::Adapter,
            message: "The agent stopped without finishing.".into(),
        })),
    };
}

fn cancel_logged(sink: &EventSink, by: CancelledBy) -> Result<(), SupervisorError> {
    sink.emit(EventBody::RunCancelled(RunCancelled { by }))
}
