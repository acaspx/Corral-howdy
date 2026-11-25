//! The monthly caps (M2 item 5; system design §7; security model T9; TP-BUD-03..04, TP-BUD-06..07).
//! Safety logic: a protected path (AGENTS.md).
//!
//! One [`SpendLedger`] is shared by every run. A request is admitted under its run's cap, its
//! agent's monthly cap and the monthly cap for all agents together, in one step under the
//! ledger's lock, so runs racing each other can't add up past a cap. Like a run's budget, the
//! ledger counts what's been billed this month plus the worst case of every admitted request that
//! hasn't reported usage yet.
//!
//! - **Months are the user's calendar months** ([`LocalMonths`]): spend rolls over at local
//!   midnight on the first. A request in flight at midnight stays held, and its cost is billed
//!   to the month its usage arrives in. A clock that goes back never reopens an old month.
//! - **Each agent's own cap** (TP-AED-01): the agent editor sets one from $0 up to the cap for
//!   all agents together, which still holds for everyone; an agent without one has
//!   [`LedgerConfig::agent_month_cap_micro_usd`]. A cap takes effect at the agent's next request.
//! - **Seeding:** this month's spend comes back from the event log at launch, replayed in order
//!   with [`SpendLedger::seed`].

use super::{
    Admission, BudgetError, MICRO_USD_PER_USD, Outer, RequestEstimate, RunBudget, Settled,
};
use crate::ids::AgentId;
use jiff::Timestamp;
use jiff::tz::{Offset, TimeZone};
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Which monthly cap refused a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonthlyCap {
    /// The agent's own cap.
    Agent,
    /// The cap for all agents together.
    All,
}

/// Why an agent's cap wasn't set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CapError {
    /// Above the cap for all agents together.
    #[error(
        "An agent's monthly cap can be at most {}, the cap for all agents together (asked for {}).",
        super::usd(*all_agents_micro_usd),
        super::usd(*cap_micro_usd)
    )]
    AboveAllAgents {
        /// The cap asked for.
        cap_micro_usd: u64,
        /// The cap for all agents together.
        all_agents_micro_usd: u64,
    },
}

/// The monthly caps. The defaults are the security model's: $25 per agent, $100 for all agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedgerConfig {
    /// Each agent's monthly cap.
    pub agent_month_cap_micro_usd: u64,
    /// The monthly cap for all agents together.
    pub global_month_cap_micro_usd: u64,
}

impl Default for LedgerConfig {
    fn default() -> Self {
        Self {
            agent_month_cap_micro_usd: 25 * MICRO_USD_PER_USD,
            global_month_cap_micro_usd: 100 * MICRO_USD_PER_USD,
        }
    }
}

/// A calendar month.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Month {
    /// The year.
    pub year: i16,
    /// The month, 1 to 12.
    pub month: i8,
}

/// Which calendar month a moment falls in, in the user's time zone.
#[derive(Debug, Clone)]
pub struct LocalMonths {
    zone: TimeZone,
}

impl LocalMonths {
    /// The Mac's time zone.
    pub fn system() -> Self {
        Self {
            zone: TimeZone::system(),
        }
    }

    /// A fixed offset from UTC, in hours (for tests and fixed-offset zones). `None` past ±25.
    pub fn fixed(hours: i8) -> Option<Self> {
        Offset::from_hours(hours).ok().map(|offset| Self {
            zone: TimeZone::fixed(offset),
        })
    }

    /// The month `ts_ms` (ms since the Unix epoch) falls in. Past the calendar's end, the last
    /// month there is.
    pub fn month(&self, ts_ms: u64) -> Month {
        let ts = i64::try_from(ts_ms)
            .ok()
            .and_then(|ms| Timestamp::from_millisecond(ms).ok())
            .unwrap_or(Timestamp::MAX);
        let local = self.zone.to_datetime(ts);
        Month {
            year: local.year(),
            month: local.month(),
        }
    }
}

#[derive(Debug, Default)]
struct State {
    /// The month being counted. Never moves back.
    month: Option<Month>,
    /// Billed this month, by agent.
    spent: HashMap<AgentId, u64>,
    /// Held for requests in flight, by agent, whatever month they were admitted in.
    held: HashMap<AgentId, u64>,
    /// Agents' own monthly caps, over the default.
    caps: HashMap<AgentId, u64>,
}

impl State {
    /// Moves to `month` if it's later than the one being counted: this month's spend starts
    /// from nothing. Holds carry over.
    fn roll(&mut self, month: Month) {
        if self.month.is_none_or(|counting| month > counting) {
            self.month = Some(month);
            self.spent.clear();
        }
    }

    fn agent_cap(&self, agent: &AgentId, config: &LedgerConfig) -> u64 {
        self.caps
            .get(agent)
            .copied()
            .unwrap_or(config.agent_month_cap_micro_usd)
    }

    fn committed(&self, agent: &AgentId) -> u64 {
        let spent = self.spent.get(agent).copied().unwrap_or(0);
        spent.saturating_add(self.held.get(agent).copied().unwrap_or(0))
    }

    fn committed_all(&self) -> u64 {
        self.spent
            .values()
            .chain(self.held.values())
            .fold(0, |sum, micro| sum.saturating_add(*micro))
    }
}

/// This month's spend, by agent and for all agents, shared by every run.
#[derive(Debug)]
pub struct SpendLedger {
    config: LedgerConfig,
    months: LocalMonths,
    state: Mutex<State>,
}

impl Default for SpendLedger {
    /// The default caps, in the Mac's time zone.
    fn default() -> Self {
        Self::new(LedgerConfig::default(), LocalMonths::system())
    }
}

impl SpendLedger {
    /// A ledger with `config`'s caps, counting the months `months` gives.
    pub fn new(config: LedgerConfig, months: LocalMonths) -> Self {
        Self {
            config,
            months,
            state: Mutex::new(State::default()),
        }
    }

    /// The caps.
    pub fn config(&self) -> LedgerConfig {
        self.config
    }

    /// Sets `agent`'s own monthly cap, from its next request on; `None` goes back to the
    /// default. A cap above the cap for all agents together is refused and changes nothing.
    pub fn set_agent_cap(
        &self,
        agent: &AgentId,
        cap_micro_usd: Option<u64>,
    ) -> Result<(), CapError> {
        let all = self.config.global_month_cap_micro_usd;
        if let Some(cap) = cap_micro_usd
            && cap > all
        {
            return Err(CapError::AboveAllAgents {
                cap_micro_usd: cap,
                all_agents_micro_usd: all,
            });
        }
        let mut state = self.lock_now();
        match cap_micro_usd {
            Some(cap) => {
                state.caps.insert(agent.clone(), cap);
            }
            None => {
                state.caps.remove(agent);
            }
        }
        Ok(())
    }

    /// `agent`'s monthly cap: its own, or the default.
    pub fn agent_cap(&self, agent: &AgentId) -> u64 {
        self.lock_now().agent_cap(agent, &self.config)
    }

    /// Admits a request from `agent`'s run at `ts_ms` under the run's limits and both monthly
    /// caps, and holds its worst case against the month. Refused: nothing is held anywhere.
    pub fn admit(
        &self,
        agent: &AgentId,
        ts_ms: u64,
        run: &mut RunBudget,
        estimate: &RequestEstimate,
    ) -> Result<Admission, BudgetError> {
        let mut state = self.lock(ts_ms);
        let agent_committed = state.committed(agent);
        let all_committed = state.committed_all();
        let agent_cap = state.agent_cap(agent, &self.config);
        let agent_room = agent_cap.saturating_sub(agent_committed);
        let all_room = self
            .config
            .global_month_cap_micro_usd
            .saturating_sub(all_committed);
        let outer = if agent_room <= all_room {
            Outer {
                room_micro_usd: agent_room,
                cap: MonthlyCap::Agent,
                cap_micro_usd: agent_cap,
                committed_micro_usd: agent_committed,
            }
        } else {
            Outer {
                room_micro_usd: all_room,
                cap: MonthlyCap::All,
                cap_micro_usd: self.config.global_month_cap_micro_usd,
                committed_micro_usd: all_committed,
            }
        };
        let admission = run.admit_under(estimate, Some(outer))?;
        let held = state.held.entry(agent.clone()).or_insert(0);
        *held = held.saturating_add(admission.worst_case_micro_usd);
        Ok(admission)
    }

    /// Gives back a hold whose request was never sent.
    pub fn release(&self, agent: &AgentId, worst_case_micro_usd: u64) {
        let mut state = self.lock_now();
        unhold(&mut state, agent, worst_case_micro_usd);
    }

    /// A request's usage arrived at `ts_ms`: its hold is replaced by what it was billed, counted
    /// in that month.
    pub fn settle(&self, agent: &AgentId, ts_ms: u64, settled: Settled) {
        let mut state = self.lock(ts_ms);
        unhold(&mut state, agent, settled.held_micro_usd);
        let spent = state.spent.entry(agent.clone()).or_insert(0);
        *spent = spent.saturating_add(settled.charged_micro_usd);
    }

    /// Counts spend from the event log at launch: `micro_usd` billed to `agent` at `ts_ms`.
    /// Replay in log order; a later month drops the months before it.
    pub fn seed(&self, agent: &AgentId, ts_ms: u64, micro_usd: u64) {
        let mut state = self.lock(ts_ms);
        let spent = state.spent.entry(agent.clone()).or_insert(0);
        *spent = spent.saturating_add(micro_usd);
    }

    /// What `agent` was billed in the month counted at `ts_ms`.
    pub fn spent(&self, agent: &AgentId, ts_ms: u64) -> u64 {
        self.lock(ts_ms).spent.get(agent).copied().unwrap_or(0)
    }

    /// `agent`'s billed spend plus its holds, at `ts_ms`.
    pub fn committed(&self, agent: &AgentId, ts_ms: u64) -> u64 {
        self.lock(ts_ms).committed(agent)
    }

    /// All agents' billed spend plus their holds, at `ts_ms`.
    pub fn committed_all(&self, ts_ms: u64) -> u64 {
        self.lock(ts_ms).committed_all()
    }

    /// The state, moved on to the month `ts_ms` falls in. A poisoned lock is taken as is: the
    /// counts in it are still the best record of spend there is, and refusing every request
    /// because of it would fail safe but strand the user.
    fn lock(&self, ts_ms: u64) -> MutexGuard<'_, State> {
        let month = self.months.month(ts_ms);
        let mut state = self.lock_now();
        state.roll(month);
        state
    }

    fn lock_now(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn unhold(state: &mut State, agent: &AgentId, micro_usd: u64) {
    if let Some(held) = state.held.get_mut(agent) {
        *held = held.saturating_sub(micro_usd);
    }
}
