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
//! - **The hard spend cap** (SK-5): each run's sink holds a [`RunBudget`] with the per-run cap
//!   from [`BudgetConfig`]. A driver asks [`EventSink::request_model`] before every model
//!   request; the budget meter ([`crate::budget`]) decides, and only an admitted request is logged
//!   as `model.request`. A refusal fails the run with `budget_hard_cap`. `usage.reported` events
//!   are counted against the cap as they're logged.
//!
//! The supervisor itself decides nothing about policy or budgets; it routes requests to the
//! meter. Runs here are independent: follow-up runs in a session ("prompt again") are wired up
//! with the session layer.
//!
//! The [`EventLog`] is called from async tasks. The SQLite store is fast enough for M1; if it
//! ever blocks noticeably, the call moves to `spawn_blocking`.

use crate::budget::{
    Admission, BudgetConfig, BudgetError, RequestEstimate, RunBudget, SpendLedger,
};
use crate::clock::Clock;
use crate::event::{
    BudgetScope, BudgetWarned, CancelledBy, Event, EventBody, FailureReason, ModelRequest,
    PauseReason, RunCancelled, RunFailed, RunPaused, RunQueued, RunResumed, RunStarted,
    SessionClosed,
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

    /// Whether `agent`'s runs stop at safe points ([`EventSink::pause_point`]), so a card can
    /// offer Pause (TP-PAUSE-01). A driver that doesn't call it says no, the default: a pause
    /// would never take hold.
    fn pauses(&self, _agent: &AgentId) -> bool {
        false
    }
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
    /// Only a run that's running can pause or resume: this one is queued or has ended.
    #[error("run {0} isn't running, so it can't pause or resume")]
    NotRunning(RunId),
    /// The budget meter refused a model request (the run has failed), or a `model.request` was
    /// emitted without going through it.
    #[error(transparent)]
    Budget(#[from] BudgetError),
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

/// One run's log state: its lifecycle, next seq and spend. Shared by its sink and the
/// supervisor.
#[derive(Debug)]
struct RunLog {
    run_id: RunId,
    /// Whose run it is: its spend counts against this agent's month.
    agent_id: AgentId,
    span_id: SpanId,
    state: Lifecycle,
    next_seq: u64,
    budget: RunBudget,
    /// `budget.warned` was logged for the soft limit (once per threshold).
    warned_soft: bool,
    /// True while the user wants the run paused; its driver pauses at its next safe point
    /// ([`EventSink::pause_point`]).
    pause: watch::Sender<bool>,
}

/// Emits a run's events: assigns `seq` and `ts`, enforces the lifecycle, appends to the log.
#[derive(Clone)]
pub struct EventSink {
    run: Arc<Mutex<RunLog>>,
    log: Arc<dyn EventLog>,
    clock: Arc<dyn Clock>,
    /// This month's spend across every run (the monthly caps).
    ledger: Arc<SpendLedger>,
}

impl EventSink {
    /// Emits `body` as the run's next event. Refused (and not logged) if the lifecycle doesn't
    /// allow it now. `model.request` is refused here: it goes through [`Self::request_model`].
    pub fn emit(&self, body: EventBody) -> Result<(), SupervisorError> {
        if let EventBody::ModelRequest(_) = &body {
            return Err(BudgetError::NotAdmitted.into());
        }
        let mut run = self.run.lock().map_err(|_| SupervisorError::Poisoned)?;
        self.append(&mut run, body)
    }

    /// Asks the budget meter to admit a model request, before anything is sent. Admitted: logs
    /// `model.request` and returns the output limit the request must carry. Refused: fails the
    /// run with `budget_hard_cap` and returns the refusal; send nothing.
    pub fn request_model(
        &self,
        request: ModelRequest,
        estimate: &RequestEstimate,
    ) -> Result<Admission, SupervisorError> {
        let mut run = self.run.lock().map_err(|_| SupervisorError::Poisoned)?;
        let event = self.event(&run, EventBody::ModelRequest(request));
        // A run that can't log a request now (cancelled, finished) is refused as such, not
        // charged.
        next(&run.state, &event)?;
        // Under the run's limits and both monthly caps at once, so racing runs can't overshoot.
        let agent = run.agent_id.clone();
        let admission = match self
            .ledger
            .admit(&agent, event.ts, &mut run.budget, estimate)
        {
            Ok(admission) => admission,
            // The soft limit doesn't end the run: the driver asks the user (a
            // `budget.threshold` checkpoint) and calls `raise_to_cap` or stops.
            Err(refusal @ BudgetError::SoftLimit { .. }) => {
                if !run.warned_soft {
                    let warned = EventBody::BudgetWarned(BudgetWarned {
                        scope: BudgetScope::Run,
                        spent_micro_usd: run.budget.committed(),
                        limit_micro_usd: run.budget.soft_limit().unwrap_or(run.budget.cap()),
                    });
                    self.append(&mut run, warned)?;
                    run.warned_soft = true;
                }
                return Err(refusal.into());
            }
            Err(refusal) => {
                self.append(
                    &mut run,
                    EventBody::RunFailed(RunFailed {
                        reason: FailureReason::BudgetHardCap,
                        message: refusal.to_string(),
                    }),
                )?;
                return Err(refusal.into());
            }
        };
        if let Err(e) = self.append(&mut run, event.body) {
            // Not logged, so not sent: give the reservation back.
            run.budget.release_last();
            self.ledger.release(&agent, admission.worst_case_micro_usd);
            return Err(e);
        }
        Ok(admission)
    }

    /// The user approved going past the soft limit: for the rest of the run, requests are
    /// admitted up to the hard cap. Nothing changes for a run without a soft limit.
    pub fn raise_to_cap(&self) -> Result<(), SupervisorError> {
        let mut run = self.run.lock().map_err(|_| SupervisorError::Poisoned)?;
        run.budget.approve_soft_limit();
        Ok(())
    }

    /// What was spent and the run's limits, for the soft-limit checkpoint's text:
    /// `(committed, soft limit, cap)`.
    pub fn budget_status(&self) -> Result<(u64, Option<u64>, u64), SupervisorError> {
        let run = self.run.lock().map_err(|_| SupervisorError::Poisoned)?;
        Ok((
            run.budget.committed(),
            run.budget.soft_limit(),
            run.budget.cap(),
        ))
    }

    fn event(&self, run: &RunLog, body: EventBody) -> Event {
        Event {
            run_id: run.run_id.clone(),
            seq: run.next_seq,
            ts: self.clock.now_ms(),
            span_id: run.span_id.clone(),
            parent_span_id: None,
            body,
        }
    }

    /// Checks `body` against the lifecycle, logs it, and counts reported usage against the cap.
    fn append(&self, run: &mut RunLog, body: EventBody) -> Result<(), SupervisorError> {
        let event = self.event(run, body);
        let state = next(&run.state, &event)?;
        self.log.append(&event)?;
        run.state = state;
        run.next_seq += 1;
        if let EventBody::UsageReported(usage) = &event.body {
            let settled = run.budget.reconcile(usage);
            self.ledger.settle(&run.agent_id, event.ts, settled);
        }
        Ok(())
    }

    /// A safe point to pause at: drivers call it between steps, never mid-request or mid-tool.
    /// If the user asked to pause, it logs `run.paused` and waits, logging nothing else, until
    /// Resume (it logs `run.resumed`) or Stop. Returns whether to carry on: false once Stop has
    /// fired, and the driver returns; the supervisor logs `run.cancelled`.
    pub async fn pause_point(&self, cancel: &CancelToken) -> bool {
        if cancel.is_cancelled() {
            return false;
        }
        let Ok(mut wanted) = self.run.lock().map(|run| run.pause.subscribe()) else {
            return true;
        };
        if !*wanted.borrow_and_update() {
            return true;
        }
        // Refused while a checkpoint is open or the run is ending: carry on, and the pause
        // takes hold at the next safe point.
        if self
            .emit(EventBody::RunPaused(RunPaused {
                reason: PauseReason::User,
            }))
            .is_err()
        {
            return !cancel.is_cancelled();
        }
        loop {
            tokio::select! {
                changed = wanted.changed() => {
                    // The sender lives in this run's log, which outlives its driver.
                    if changed.is_err() {
                        return false;
                    }
                    if !*wanted.borrow_and_update() {
                        break;
                    }
                }
                () = cancel.cancelled() => return false,
            }
        }
        self.emit(EventBody::RunResumed(RunResumed {})).is_ok()
    }

    /// Asks the run to pause (`true`) or carry on (`false`).
    fn want_paused(&self, paused: bool) -> Result<(), SupervisorError> {
        self.run
            .lock()
            .map_err(|_| SupervisorError::Poisoned)?
            .pause
            .send_replace(paused);
        Ok(())
    }

    /// The run's phase now.
    pub fn phase(&self) -> Result<Phase, SupervisorError> {
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
    budget: BudgetConfig,
    /// The monthly caps, shared by every run.
    ledger: Arc<SpendLedger>,
    /// The soft limit for runs deployed from now on (TP-BUD-02), if any.
    soft_limit: Mutex<Option<u64>>,
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
    /// A supervisor that runs `driver` and logs to `log`, with time from `clock`, and the
    /// default budget ([`BudgetConfig::default`]: $1 per run).
    pub fn new(
        config: SupervisorConfig,
        driver: Arc<dyn RunDriver>,
        log: Arc<dyn EventLog>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self::with_budget(config, BudgetConfig::default(), driver, log, clock)
    }

    /// [`Self::new`] with these budget settings, and the default monthly caps in the Mac's time
    /// zone.
    pub fn with_budget(
        config: SupervisorConfig,
        budget: BudgetConfig,
        driver: Arc<dyn RunDriver>,
        log: Arc<dyn EventLog>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let ledger = Arc::new(SpendLedger::default());
        Self::with_ledger(config, budget, ledger, driver, log, clock)
    }

    /// [`Self::with_budget`] with this ledger for the monthly caps (seeded from the log at
    /// launch).
    pub fn with_ledger(
        config: SupervisorConfig,
        budget: BudgetConfig,
        ledger: Arc<SpendLedger>,
        driver: Arc<dyn RunDriver>,
        log: Arc<dyn EventLog>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self(Arc::new(Inner {
            config,
            budget,
            ledger,
            soft_limit: Mutex::new(None),
            driver,
            log,
            clock,
            state: Mutex::new(State::default()),
            idle: Notify::new(),
        }))
    }

    /// The ledger every run's spend counts in.
    pub fn ledger(&self) -> &SpendLedger {
        &self.0.ledger
    }

    /// Sets the soft limit for runs deployed from now on (`None`: no soft limit). One at or over
    /// the cap is none. Only drivers that can ask the user should run with one: a driver that
    /// can't must end the run when it's reached.
    pub fn set_soft_limit(&self, soft_micro_usd: Option<u64>) {
        let mut soft = self
            .0
            .soft_limit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *soft = soft_micro_usd;
    }

    /// Logs `run.queued`, then starts the run now or queues it behind the concurrency limit.
    pub fn deploy(&self, request: RunRequest) -> Result<(), SupervisorError> {
        let mut state = self.lock()?;
        if state.known.contains_key(&request.run_id) {
            return Err(SupervisorError::DuplicateRun(request.run_id));
        }
        let mut budget = RunBudget::new(self.0.budget.per_run_cap_micro_usd);
        let soft = *self
            .0
            .soft_limit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(soft) = soft {
            budget = budget.with_soft_limit(soft);
        }
        let sink = EventSink {
            run: Arc::new(Mutex::new(RunLog {
                run_id: request.run_id.clone(),
                agent_id: request.agent_id.clone(),
                span_id: SpanId::from(format!("{}:invoke_agent", request.run_id).as_str()),
                state: Lifecycle::idle(),
                next_seq: 1,
                budget,
                warned_soft: false,
                pause: watch::Sender::new(false),
            })),
            log: self.0.log.clone(),
            clock: self.0.clock.clone(),
            ledger: Arc::clone(&self.0.ledger),
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

    /// Pause (the hover card's Pause): the run stops at its driver's next safe point, between
    /// steps, and logs `run.paused` there. Only a running run can pause; pausing one that's
    /// paused, or about to be, does nothing.
    pub fn pause(&self, run_id: &RunId) -> Result<(), SupervisorError> {
        self.running_sink(run_id)?.want_paused(true)
    }

    /// Resume: a paused run logs `run.resumed` and carries on; one that hadn't reached its safe
    /// point yet never pauses. Resuming a running run does nothing.
    pub fn resume(&self, run_id: &RunId) -> Result<(), SupervisorError> {
        self.running_sink(run_id)?.want_paused(false)
    }

    /// The sink of a run that's running (or paused): not queued, not ended.
    fn running_sink(&self, run_id: &RunId) -> Result<EventSink, SupervisorError> {
        let state = self.lock()?;
        match state.active.get(run_id) {
            // A run that has just ended may still be here for a moment; asking it to pause is
            // harmless, since its driver has no safe point left.
            Some(active) => Ok(active.sink.clone()),
            None if state.known.contains_key(run_id) => {
                Err(SupervisorError::NotRunning(run_id.clone()))
            }
            None => Err(SupervisorError::UnknownRun(run_id.clone())),
        }
    }

    /// Send back: logs `session.closed` for a run that has ended, returning its agent to the
    /// corral. Refused (and not logged) while the run is still going.
    pub fn close_session(&self, run_id: &RunId) -> Result<(), SupervisorError> {
        let sink = self
            .lock()?
            .known
            .get(run_id)
            .cloned()
            .ok_or_else(|| SupervisorError::UnknownRun(run_id.clone()))?;
        sink.emit(EventBody::SessionClosed(SessionClosed {}))
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

    /// Whether `agent`'s runs can pause: its driver stops at safe points (TP-PAUSE-03).
    pub fn can_pause(&self, agent: &AgentId) -> bool {
        self.0.driver.pauses(agent)
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
