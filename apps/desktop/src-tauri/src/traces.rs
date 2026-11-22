//! The trace viewer's commands (M4 item 5, part 2, TP-TV-11, 12). The corral opens the event
//! store and attaches it here; the main window asks for the newest runs and for one run's span
//! tree, which the core projects (`corral_core::trace_view`), with the run's events as JSON to
//! copy. Each answer is one query. Before the store is open, both say so.

use corral_adapters::catalog::{agent_for, provider_of};
use corral_core::agent_text::AgentText;
use corral_core::event::Event;
use corral_core::ids::RunId;
use corral_core::trace_view::{TraceView, trace_view};
use corral_store::{RunOutcome, Store};
use serde::Serialize;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use tauri::State;

/// How many runs the list shows.
pub const RECENT_RUNS: usize = 100;

/// The most characters of a prompt the list shows.
pub const MAX_PROMPT_CHARS: usize = 200;

/// One run in the viewer's list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct RunListItem {
    /// The run.
    pub run_id: String,
    /// Its agent's name, e.g. "Grok", or its ID when Corral doesn't know it.
    pub agent_name: String,
    /// The prompt, cleaned and capped.
    pub prompt: String,
    /// When it was queued, in ms since the epoch.
    #[ts(type = "number")]
    pub queued_ms: u64,
    /// When it ended, if it has.
    #[ts(type = "number | null")]
    pub ended_ms: Option<u64>,
    /// How it ended, or that it hasn't.
    pub outcome: RunListOutcome,
    /// What it cost, in micro-USD.
    #[ts(type = "number")]
    pub cost_micro_usd: u64,
}

/// One run's trace: its span tree, and its events as JSON for "Copy as JSON".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct RunTrace {
    /// The span tree.
    pub view: TraceView,
    /// The run's events, as the log holds them, indented.
    pub json: String,
}

/// How a listed run ended, as the window shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum RunListOutcome {
    /// Still going.
    Running,
    /// Finished.
    Completed,
    /// Failed.
    Failed,
    /// Stopped.
    Cancelled,
}

fn outcome(outcome: RunOutcome) -> RunListOutcome {
    match outcome {
        RunOutcome::Running => RunListOutcome::Running,
        RunOutcome::Completed => RunListOutcome::Completed,
        RunOutcome::Failed => RunListOutcome::Failed,
        RunOutcome::Cancelled => RunListOutcome::Cancelled,
    }
}

/// The event store, once the corral has opened it.
#[derive(Default)]
pub struct Traces {
    store: OnceLock<Arc<Store>>,
    /// The run a click in the corral asked to open, until the viewer takes it (TP-TRACE-05).
    open: Mutex<Option<RunId>>,
}

/// Run in the main window after a click in the corral asks for a run: it tells the viewer to
/// take it. A fixed script that carries no data; the run comes through `take_open_run`.
pub const OPEN_RUN_SCRIPT: &str = "window.dispatchEvent(new Event('corral-open-run'))";

impl Traces {
    /// Asks the viewer to open `run` (a task card clicked in the trace tree). A later click
    /// replaces an earlier one not yet taken.
    pub fn open_run(&self, run: RunId) {
        *self.open.lock().unwrap_or_else(PoisonError::into_inner) = Some(run);
    }

    /// The run asked for, once.
    pub fn take_open_run(&self) -> Option<String> {
        self.open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .map(|run| run.as_str().to_owned())
    }

    /// Lets the viewer read `store`. The first store attached stays.
    pub fn attach(&self, store: Arc<Store>) {
        if self.store.set(store).is_err() {
            eprintln!("corral: the trace viewer already has an event store");
        }
    }

    fn store(&self) -> Result<&Store, String> {
        self.store.get().map(AsRef::as_ref).ok_or_else(|| {
            "The event store isn't open yet. Traces show once the corral is running.".to_owned()
        })
    }

    /// The newest runs, newest first.
    pub fn recent_runs(&self) -> Result<Vec<RunListItem>, String> {
        let runs = self
            .store()?
            .recent_runs(RECENT_RUNS)
            .map_err(|e| format!("Couldn't read the runs: {e}"))?;
        Ok(runs
            .into_iter()
            .map(|run| RunListItem {
                agent_name: provider_of(&run.agent_id)
                    .map(|p| agent_for(p).name)
                    .unwrap_or_else(|| {
                        AgentText::new(run.agent_id.as_str(), MAX_PROMPT_CHARS)
                            .as_str()
                            .to_owned()
                    }),
                run_id: run.run_id.as_str().to_owned(),
                prompt: AgentText::new(&run.prompt, MAX_PROMPT_CHARS)
                    .as_str()
                    .to_owned(),
                queued_ms: run.queued_ms,
                ended_ms: run.ended_ms,
                outcome: outcome(run.outcome),
                cost_micro_usd: run.cost_micro_usd,
            })
            .collect())
    }

    /// One run's span tree and its events as JSON.
    pub fn run_trace(&self, run_id: &str) -> Result<RunTrace, String> {
        let events: Vec<Event> = self
            .store()?
            .read_run(&RunId::from(run_id))
            .map_err(|e| format!("Couldn't read the run: {e}"))?;
        if events.is_empty() {
            return Err(format!(
                "No run {} in the log.",
                AgentText::new(run_id, 80).as_str()
            ));
        }
        let json = serde_json::to_string_pretty(&events)
            .map_err(|e| format!("Couldn't write the run as JSON: {e}"))?;
        Ok(RunTrace {
            view: trace_view(&events),
            json,
        })
    }
}

// Async so reading the log never blocks the main thread.

/// Command: the newest runs.
#[tauri::command(async)]
pub fn recent_runs(traces: State<'_, Arc<Traces>>) -> Result<Vec<RunListItem>, String> {
    traces.recent_runs()
}

/// Command: one run's span tree and its events as JSON.
#[tauri::command(async)]
pub fn run_trace(traces: State<'_, Arc<Traces>>, run_id: String) -> Result<RunTrace, String> {
    traces.run_trace(&run_id)
}

/// Command: the run a click in the corral asked to open, once (TP-TRACE-05).
#[tauri::command(async)]
pub fn take_open_run(traces: State<'_, Arc<Traces>>) -> Option<String> {
    traces.take_open_run()
}
