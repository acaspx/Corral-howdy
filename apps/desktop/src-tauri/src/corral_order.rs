//! The corral's order and shown set in the app (M4 item 2, part 2, TP-USAGE-06..09). The core
//! decides (`corral_core::usage_rank`); this keeps its `UsageOrder` and `ShownSet`, saved as
//! `corral-order.json`, feeds them the deploys, and arranges the roster before it reaches Swift:
//! the shown agents, most used first. Swift only maps rank to slot.
//!
//! - **At launch** a new order is published, seeded with the last 90 days of deploys from the log,
//!   before anything can be under the pointer.
//! - **While Corral runs** a deploy counts at once, but a new order waits for the corral to be idle:
//!   Swift's last report was idle (no hover, no card opening or open) and no checkpoint waits on
//!   the user ([`CorralOrder::settle`], TP-USAGE-12). Until Swift reports, Rust assumes busy.
//!   Agents join (at the end) and leave at once.
//! - **A 9th agent** stays hidden and asks the user to choose ([`Placed::choose_shown`]).
//! - **Failing safe:** a saved order that can't be read starts afresh and says why
//!   ([`CorralOrder::problem`]); it's replaced on the next save. Without an app data folder it
//!   still works, unsaved.

use corral_core::corral_view::AgentCard;
use corral_core::ids::AgentId;
use corral_core::usage_rank::{
    Deploy, MAX_SHOWN, ShownChange, ShownSet, UsageOrder, WINDOW_MS, rank,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The largest saved order read. The real one is well under 1 KB.
pub const MAX_ORDER_FILE_BYTES: usize = 16 * 1024;

/// The saved file's version.
const VERSION: u32 = 1;

/// The most agents a saved file may list.
const MAX_SAVED_AGENTS: usize = 256;

/// `corral-order.json`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Saved {
    v: u32,
    order: Vec<String>,
    keep_fixed: bool,
    known: Vec<String>,
    shown: Vec<String>,
}

/// What placing the agents gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    /// The shown agents, most used first: the roster Swift gets.
    pub cards: Vec<AgentCard>,
    /// An agent was added to a full corral: the user should choose which 8 show
    /// (`corral.choose_shown`).
    pub choose_shown: bool,
}

/// The corral's order and shown set, as the app keeps them.
#[derive(Debug)]
pub struct CorralOrder {
    path: Option<PathBuf>,
    usage: UsageOrder,
    shown: ShownSet,
    deploys: Vec<Deploy>,
    problem: Option<String>,
    dirty: bool,
    /// Swift's last report: the corral is idle. False until Swift reports.
    corral_idle: bool,
}

fn agent_ids(ids: Vec<String>) -> Vec<AgentId> {
    ids.into_iter()
        .map(|id| AgentId::from(id.as_str()))
        .collect()
}

fn names(ids: &[AgentId]) -> Vec<String> {
    ids.iter().map(|id| id.as_str().to_owned()).collect()
}

/// Reads the saved order: nothing when there's no file, an error saying why when it can't be used.
fn load(path: &Path) -> Result<Option<Saved>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if bytes.len() > MAX_ORDER_FILE_BYTES {
        return Err(format!(
            "the file is {} bytes; the most is 16 KB",
            bytes.len()
        ));
    }
    let saved: Saved = serde_json::from_slice(&bytes)
        .map_err(|e| format!("it isn't the file Corral writes ({e})"))?;
    if saved.v != VERSION {
        return Err(format!(
            "it's version {}; this Corral reads version {VERSION}",
            saved.v
        ));
    }
    if [&saved.order, &saved.known, &saved.shown]
        .iter()
        .any(|list| list.len() > MAX_SAVED_AGENTS)
    {
        return Err(format!("it lists more than {MAX_SAVED_AGENTS} agents"));
    }
    Ok(Some(saved))
}

impl CorralOrder {
    /// The order saved at `path`, or a fresh one (and the reason) when it can't be read. With no
    /// path, nothing is saved.
    pub fn open(path: Option<PathBuf>) -> Self {
        let (saved, problem) = match path.as_deref().map(load) {
            None | Some(Ok(None)) => (None, None),
            Some(Ok(Some(saved))) => (Some(saved), None),
            Some(Err(reason)) => (
                None,
                Some(format!(
                    "Couldn't read the corral's saved order, so it starts afresh: {reason}."
                )),
            ),
        };
        let (usage, shown) = match saved {
            Some(saved) => (
                UsageOrder::new(agent_ids(saved.order), saved.keep_fixed),
                ShownSet::restore(agent_ids(saved.known), agent_ids(saved.shown)),
            ),
            None => (UsageOrder::new(Vec::new(), false), ShownSet::default()),
        };
        Self {
            path,
            usage,
            shown,
            deploys: Vec::new(),
            problem,
            dirty: false,
            corral_idle: false,
        }
    }

    /// Why the saved order couldn't be read, if it couldn't.
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// Whether "Keep order fixed" is on.
    pub fn keeps_fixed(&self) -> bool {
        self.usage.keeps_fixed()
    }

    /// Whether a new order is waiting for the corral to be idle.
    pub fn has_pending(&self) -> bool {
        self.usage.has_pending()
    }

    /// Whether an agent was added to a full corral and the user hasn't chosen yet.
    pub fn needs_choice(&self) -> bool {
        self.shown.needs_choice()
    }

    /// How many agents aren't shown.
    pub fn hidden_count(&self) -> usize {
        self.shown.hidden_count()
    }

    /// Drops deploys too old to count, so memory doesn't grow; the score ignores them anyway.
    fn forget_old(&mut self, now_ms: u64) {
        let since = now_ms.saturating_sub(WINDOW_MS);
        self.deploys.retain(|d| d.ts_ms >= since);
    }

    /// Brings the order and the shown set in line with `cards` (the agents now, in the order the
    /// catalog lists them) and returns the roster for Swift. Agents join and leave at once; the
    /// order itself doesn't move.
    pub fn place(&mut self, cards: Vec<AgentCard>, now_ms: u64) -> Placed {
        let mut choose_shown = false;
        for known in self.shown.known().to_vec() {
            if !cards.iter().any(|c| c.id == known) {
                self.shown.remove(&known);
                self.dirty = true;
            }
        }
        for card in &cards {
            match self.shown.add(card.id.clone()) {
                ShownChange::Unchanged => {}
                ShownChange::Shown => self.dirty = true,
                ShownChange::ChooseShown => {
                    self.dirty = true;
                    choose_shown = true;
                }
            }
        }
        self.forget_old(now_ms);
        let ranked = rank(self.shown.known(), &self.deploys, now_ms);
        if self.usage.update(&ranked, false) {
            self.dirty = true;
        }
        let order = self.shown.in_order(self.usage.order());
        let mut placed: Vec<AgentCard> = order
            .iter()
            .filter_map(|id| cards.iter().find(|c| &c.id == id).cloned())
            .collect();
        placed.truncate(MAX_SHOWN);
        Placed {
            cards: placed,
            choose_shown,
        }
    }

    /// At launch: the deploys from the log, and a new order published at once. Returns whether
    /// the order changed.
    pub fn seed(&mut self, deploys: Vec<Deploy>, now_ms: u64) -> bool {
        self.deploys = deploys;
        self.forget_old(now_ms);
        let ranked = rank(self.shown.known(), &self.deploys, now_ms);
        let changed = self.usage.update(&ranked, true);
        self.dirty |= changed;
        changed
    }

    /// A deploy, as it's logged.
    pub fn deployed(&mut self, agent: AgentId, ts_ms: u64) {
        self.deploys.push(Deploy { agent, ts_ms });
    }

    /// A run ended: the order is worked out again and waits for the corral to be idle. Nothing
    /// visible changes: agents only join and leave through [`CorralOrder::place`].
    pub fn run_ended(&mut self, now_ms: u64) {
        self.forget_old(now_ms);
        let ranked = rank(self.shown.known(), &self.deploys, now_ms);
        self.usage.update(&ranked, false);
    }

    /// The corral is idle: publishes a waiting order. Returns whether it changed.
    pub fn idle(&mut self) -> bool {
        let changed = self.usage.idle();
        self.dirty |= changed;
        changed
    }

    /// Swift's report: the corral turned idle (true) or busy (false).
    pub fn set_corral_idle(&mut self, idle: bool) {
        self.corral_idle = idle;
    }

    /// Publishes a waiting order if the corral is idle: Swift's last report was idle and no
    /// checkpoint waits on the user. Checked when Swift reports, when a run ends and when a
    /// checkpoint is answered. Returns whether the order changed.
    pub fn settle(&mut self, checkpoint_waiting: bool) -> bool {
        if !self.corral_idle || checkpoint_waiting {
            return false;
        }
        self.idle()
    }

    /// Writes the order if it changed: a temporary file, then a rename. With no path, nothing.
    pub fn save(&mut self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if !self.dirty && path.exists() {
            return Ok(());
        }
        let saved = Saved {
            v: VERSION,
            order: names(self.usage.order()),
            keep_fixed: self.usage.keeps_fixed(),
            known: names(self.shown.known()),
            shown: names(self.shown.shown()),
        };
        let fail = |e: String| format!("Couldn't save the corral's order: {e}");
        let bytes = serde_json::to_vec(&saved).map_err(|e| fail(e.to_string()))?;
        let mut temp = path.clone().into_os_string();
        temp.push(".tmp");
        let temp = PathBuf::from(temp);
        std::fs::write(&temp, &bytes).map_err(|e| fail(e.to_string()))?;
        if let Err(e) = std::fs::rename(&temp, path) {
            if let Err(cleanup) = std::fs::remove_file(&temp) {
                eprintln!("corral: couldn't remove {}: {cleanup}", temp.display());
            }
            return Err(fail(e.to_string()));
        }
        self.dirty = false;
        self.problem = None;
        Ok(())
    }
}
