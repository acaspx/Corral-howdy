//! The corral's order and shown set (M4 item 2, ADR-012, docs/07 §3.1-3.2, TP-USAGE-01..05). Pure
//! logic: the app feeds it the deploys, the agents and whether the corral is idle, and hands Swift
//! the shown agents, most used first. Swift only maps rank to slot (TP-RANK-*).
//!
//! - **Score** ([`score`]): each deploy (`run.queued`) in the last 90 days counts, halved every 14
//!   days, so this week's habits count more than last month's.
//! - **Rank** ([`rank`]): by score, then most recent deploy, then the order agents were added.
//! - **Order** ([`UsageOrder`]): a new order is published only when the corral is idle (no hover,
//!   drag, open card or pending checkpoint), and two neighbors swap only when the one behind leads
//!   by at least 10% ([`reorder`]), so near-ties don't flicker. Agents join (at the end) and leave
//!   at once. "Keep order fixed" freezes the order; deploys still count.
//! - **Shown set** ([`ShownSet`]): the first 8 agents show. A 9th changes nothing and asks the user
//!   to choose (`corral.choose_shown`). Removing a shown agent never promotes a hidden one.

use crate::event::{Event, EventBody};
use crate::ids::AgentId;

/// One day, in ms.
pub const DAY_MS: u64 = 86_400_000;

/// How far back deploys count: 90 days.
pub const WINDOW_MS: u64 = 90 * DAY_MS;

/// How long a deploy takes to count half as much: 14 days.
pub const HALF_LIFE_MS: u64 = 14 * DAY_MS;

/// How far ahead the agent behind must be to swap with its neighbor: 10%.
pub const HYSTERESIS: f64 = 1.10;

/// The most agents the corral shows: an octagon.
pub const MAX_SHOWN: usize = 8;

/// One deploy: an agent given a task at a moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deploy {
    /// The agent.
    pub agent: AgentId,
    /// When its run was queued.
    pub ts_ms: u64,
}

/// The deploys in `events`: their `run.queued`.
pub fn deploys(events: &[Event]) -> Vec<Deploy> {
    events
        .iter()
        .filter_map(|event| {
            let EventBody::RunQueued(queued) = &event.body else {
                return None;
            };
            Some(Deploy {
                agent: queued.agent_id.clone(),
                ts_ms: event.ts,
            })
        })
        .collect()
}

/// What `deploys` add up to at `now_ms`: each in the last 90 days, halved every 14 days. A
/// deploy from a clock running ahead counts as now.
pub fn score(deploys: &[Deploy], now_ms: u64) -> f64 {
    deploys
        .iter()
        .map(|d| now_ms.saturating_sub(d.ts_ms))
        .filter(|age| *age <= WINDOW_MS)
        .map(|age| 0.5_f64.powf(age as f64 / HALF_LIFE_MS as f64))
        .sum()
}

/// An agent's place by usage.
#[derive(Debug, Clone, PartialEq)]
pub struct Ranked {
    /// The agent.
    pub agent: AgentId,
    /// Its score.
    pub score: f64,
    /// Its most recent deploy, if any.
    pub last_deploy_ms: Option<u64>,
}

/// `agents` (in the order they were added) by usage: score, then most recent deploy, then the
/// order added. Deploys of other agents are ignored.
pub fn rank(agents: &[AgentId], deploys: &[Deploy], now_ms: u64) -> Vec<Ranked> {
    let mut ranked: Vec<(usize, Ranked)> = agents
        .iter()
        .enumerate()
        .map(|(added, agent)| {
            let own: Vec<Deploy> = deploys
                .iter()
                .filter(|d| &d.agent == agent)
                .cloned()
                .collect();
            let ranked = Ranked {
                agent: agent.clone(),
                score: score(&own, now_ms),
                last_deploy_ms: own.iter().map(|d| d.ts_ms).max(),
            };
            (added, ranked)
        })
        .collect();
    ranked.sort_by(|(a_added, a), (b_added, b)| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| b.last_deploy_ms.cmp(&a.last_deploy_ms))
            .then_with(|| a_added.cmp(b_added))
    });
    ranked.into_iter().map(|(_, r)| r).collect()
}

/// Whether `behind` leads `ahead` by enough to swap: more, and by at least [`HYSTERESIS`]. The
/// tiny allowance keeps a lead of exactly 10% from losing to rounding.
fn passes(behind: f64, ahead: f64) -> bool {
    behind > ahead && behind >= ahead * HYSTERESIS * (1.0 - 1e-9)
}

/// The agents in `published` that are still in `ranked`, then the new ones in rank order.
fn members(published: &[AgentId], ranked: &[Ranked]) -> Vec<AgentId> {
    let mut order: Vec<AgentId> = published
        .iter()
        .filter(|a| ranked.iter().any(|r| &r.agent == *a))
        .cloned()
        .collect();
    for r in ranked {
        if !order.contains(&r.agent) {
            order.push(r.agent.clone());
        }
    }
    order
}

/// `published` moved toward `ranked` with hysteresis: starting from the published order (new
/// agents at the end), a neighbor moves up past another only when it leads by at least 10%, until
/// none does. Applying it again changes nothing.
pub fn reorder(published: &[AgentId], ranked: &[Ranked]) -> Vec<AgentId> {
    let mut order = members(published, ranked);
    let score_of = |agent: &AgentId| {
        ranked
            .iter()
            .find(|r| &r.agent == agent)
            .map(|r| r.score)
            .unwrap_or_default()
    };
    // Each swap moves a strictly higher score ahead, undoing one inversion, so this ends within
    // n² passes; the bound is a backstop.
    for _ in 0..=order.len().pow(2) {
        let mut swapped = false;
        for i in 1..order.len() {
            if passes(score_of(&order[i]), score_of(&order[i - 1])) {
                order.swap(i - 1, i);
                swapped = true;
            }
        }
        if !swapped {
            break;
        }
    }
    order
}

/// The corral's published order, and one waiting for the corral to be idle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageOrder {
    published: Vec<AgentId>,
    pending: Option<Vec<AgentId>>,
    keep_fixed: bool,
}

impl UsageOrder {
    /// Starting from `published` (as saved), with "Keep order fixed" on or off.
    pub fn new(published: Vec<AgentId>, keep_fixed: bool) -> Self {
        Self {
            published,
            pending: None,
            keep_fixed,
        }
    }

    /// The order the corral shows, most used first.
    pub fn order(&self) -> &[AgentId] {
        &self.published
    }

    /// Whether a new order is waiting for the corral to be idle.
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Whether "Keep order fixed" is on.
    pub fn keeps_fixed(&self) -> bool {
        self.keep_fixed
    }

    /// Turns "Keep order fixed" on or off. On drops any waiting order.
    pub fn set_keep_fixed(&mut self, keep_fixed: bool) {
        self.keep_fixed = keep_fixed;
        if keep_fixed {
            self.pending = None;
        }
    }

    /// Takes in a new ranking. Agents join and leave at once; a new order is published now if the
    /// corral is `idle`, and otherwise waits for [`UsageOrder::idle`]. Returns whether the
    /// published order changed.
    pub fn update(&mut self, ranked: &[Ranked], idle: bool) -> bool {
        let members = members(&self.published, ranked);
        let joined_or_left = members != self.published;
        if self.keep_fixed {
            self.published = members;
            return joined_or_left;
        }
        let target = reorder(&members, ranked);
        if idle {
            self.pending = None;
            let changed = target != self.published;
            self.published = target;
            return changed;
        }
        self.pending = (target != members).then_some(target);
        self.published = members;
        joined_or_left
    }

    /// The corral is idle: publishes the waiting order, if any. Returns whether the order changed.
    /// A waiting order always differs from the published one ([`UsageOrder::update`]).
    pub fn idle(&mut self) -> bool {
        let Some(order) = self.pending.take() else {
            return false;
        };
        self.published = order;
        true
    }
}

/// What adding an agent did to the shown set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShownChange {
    /// It was already there.
    Unchanged,
    /// It shows.
    Shown,
    /// The corral is full: nothing changed, and the user chooses which 8 show
    /// (`corral.choose_shown`).
    ChooseShown,
}

/// Why a choice of shown agents was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShownError {
    /// More than [`MAX_SHOWN`].
    #[error("the corral holds up to 8 agents; {0} were picked")]
    TooMany(usize),
    /// Not one of the agents.
    #[error("{0} isn't one of your agents")]
    Unknown(AgentId),
}

/// Which agents the corral shows, of all the user has.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShownSet {
    known: Vec<AgentId>,
    shown: Vec<AgentId>,
    choosing: bool,
}

impl ShownSet {
    /// As saved: every agent, and the ones shown (kept only if still an agent, at most 8).
    pub fn restore(known: Vec<AgentId>, shown: Vec<AgentId>) -> Self {
        let mut kept: Vec<AgentId> = Vec::new();
        for agent in shown {
            if known.contains(&agent) && !kept.contains(&agent) && kept.len() < MAX_SHOWN {
                kept.push(agent);
            }
        }
        Self {
            known,
            shown: kept,
            choosing: false,
        }
    }

    /// Adds an agent. It shows if there's room; otherwise nothing changes and the user chooses.
    pub fn add(&mut self, agent: AgentId) -> ShownChange {
        if self.known.contains(&agent) {
            return ShownChange::Unchanged;
        }
        self.known.push(agent.clone());
        if self.shown.len() < MAX_SHOWN {
            self.shown.push(agent);
            ShownChange::Shown
        } else {
            self.choosing = true;
            ShownChange::ChooseShown
        }
    }

    /// Removes an agent. The corral shrinks; no hidden agent takes its place.
    pub fn remove(&mut self, agent: &AgentId) {
        self.known.retain(|a| a != agent);
        self.shown.retain(|a| a != agent);
    }

    /// The user's choice of which agents show: at most 8, each one of theirs; repeats count once.
    pub fn choose(&mut self, picks: &[AgentId]) -> Result<(), ShownError> {
        let mut chosen: Vec<AgentId> = Vec::new();
        for agent in picks {
            if !chosen.contains(agent) {
                chosen.push(agent.clone());
            }
        }
        if chosen.len() > MAX_SHOWN {
            return Err(ShownError::TooMany(chosen.len()));
        }
        if let Some(stranger) = chosen.iter().find(|a| !self.known.contains(a)) {
            return Err(ShownError::Unknown(stranger.clone()));
        }
        self.shown = chosen;
        self.choosing = false;
        Ok(())
    }

    /// Every agent, in the order added.
    pub fn known(&self) -> &[AgentId] {
        &self.known
    }

    /// The agents shown.
    pub fn shown(&self) -> &[AgentId] {
        &self.shown
    }

    /// Whether `agent` is shown.
    pub fn is_shown(&self, agent: &AgentId) -> bool {
        self.shown.contains(agent)
    }

    /// Whether `agent` is one of the user's agents.
    pub fn is_known(&self, agent: &AgentId) -> bool {
        self.known.contains(agent)
    }

    /// Whether a 9th agent is waiting for the user to choose.
    pub fn needs_choice(&self) -> bool {
        self.choosing
    }

    /// How many agents aren't shown.
    pub fn hidden_count(&self) -> usize {
        self.known.len().saturating_sub(self.shown.len())
    }

    /// The shown agents in `order` (most used first): what Swift gets.
    pub fn in_order(&self, order: &[AgentId]) -> Vec<AgentId> {
        order.iter().filter(|a| self.is_shown(a)).cloned().collect()
    }
}
