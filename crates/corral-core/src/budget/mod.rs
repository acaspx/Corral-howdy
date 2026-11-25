//! The budget meter's hard-cap half (system design §7; security model T9, T25). Safety logic:
//! a protected path (AGENTS.md).
//!
//! Every model request is admitted here before it's sent. The meter adds up the request's worst
//! case: the input (bounded before the call), the vendor tool fees for the call, and the most
//! output the request may produce. If even the smallest useful answer ([`MIN_OUTPUT_TOKENS`])
//! wouldn't fit under the cap, the request is refused. Otherwise it gets the most output tokens
//! the remaining budget pays for, up to the configured ceiling, so the provider can't overshoot
//! (T25).
//!
//! - **Committed spend** is what's been billed plus the worst case of every admitted request that
//!   hasn't reported usage yet, so requests in flight can't add up past the cap.
//! - **Reconciliation:** `usage.reported` releases the oldest reservation and adds the billed
//!   cost. A cost that's missing or lower than tokens × the admitted price is charged at tokens ×
//!   price (plus the call's tool fees), so spend is never under-counted.
//! - The [`crate::supervisor::EventSink`] owns one [`RunBudget`] per run: `model.request` is
//!   logged only through its `request_model`, and a refusal fails the run with `budget_hard_cap`.
//!
//! - **The monthly caps** ([`ledger`]): every request is also admitted under its agent's monthly
//!   cap and the monthly cap for all agents together, through one [`SpendLedger`] shared by every
//!   run (TP-BUD-03..04, TP-BUD-06..07).
//!
//! Money is in micro-USD (10^-6 USD), like `usage.reported`. The versioned price table comes with
//! the first real provider (M3).

pub mod ledger;

pub use ledger::{LedgerConfig, LocalMonths, Month, MonthlyCap, SpendLedger};

use crate::event::UsageReported;
use std::collections::VecDeque;

/// Micro-USD in a dollar.
pub const MICRO_USD_PER_USD: u64 = 1_000_000;

/// The smallest answer worth asking for. A request that can't afford this many output tokens is
/// refused rather than sent with a uselessly small limit.
pub const MIN_OUTPUT_TOKENS: u32 = 16;

/// Tokens added to every input bound for the provider's own framing (roles, separators, any
/// system text it adds).
pub const REQUEST_OVERHEAD_TOKENS: u64 = 256;

/// An upper bound on the input tokens for `text`: a token is at least one byte in every
/// tokenizer Corral talks to, so the byte count can only over-estimate.
pub fn input_token_bound(text: &str) -> u64 {
    u64::try_from(text.len())
        .unwrap_or(u64::MAX)
        .saturating_add(REQUEST_OVERHEAD_TOKENS)
}

/// A model's list price, per million tokens, in micro-USD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Price {
    /// Per million input tokens.
    pub input_micro_usd_per_mtok: u64,
    /// Per million output tokens, reasoning included.
    pub output_micro_usd_per_mtok: u64,
    /// A higher rate for a request whose prompt is long enough (TP-BUD-08), if the model has one.
    pub long_prompt: Option<LongPrompt>,
}

/// The rate a request pays when its prompt has at least [`Self::from_input_tokens`] tokens: every
/// token of that request, output included, is priced at it (Grok 4.7 doubles from 200,000).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LongPrompt {
    /// The smallest prompt, in input tokens, that pays this rate.
    pub from_input_tokens: u64,
    /// Per million input tokens.
    pub input_micro_usd_per_mtok: u64,
    /// Per million output tokens, reasoning included.
    pub output_micro_usd_per_mtok: u64,
}

impl Price {
    /// A price in micro-USD per million tokens: `$2` per million is `2_000_000`.
    pub const fn per_million_tokens(input: u64, output: u64) -> Self {
        Self {
            input_micro_usd_per_mtok: input,
            output_micro_usd_per_mtok: output,
            long_prompt: None,
        }
    }

    /// This price, with `input` and `output` per million tokens for a request whose prompt has
    /// at least `from_input_tokens` tokens.
    pub const fn with_long_prompt(self, from_input_tokens: u64, input: u64, output: u64) -> Self {
        Self {
            long_prompt: Some(LongPrompt {
                from_input_tokens,
                input_micro_usd_per_mtok: input,
                output_micro_usd_per_mtok: output,
            }),
            ..self
        }
    }

    /// The flat rate a request with `input_tokens` of prompt pays: the long rate from its
    /// threshold on, each part never below the short rate.
    pub fn for_prompt(&self, input_tokens: u64) -> Price {
        let flat = Price::per_million_tokens(
            self.input_micro_usd_per_mtok,
            self.output_micro_usd_per_mtok,
        );
        match self.long_prompt {
            Some(long) if input_tokens >= long.from_input_tokens => Price::per_million_tokens(
                flat.input_micro_usd_per_mtok
                    .max(long.input_micro_usd_per_mtok),
                flat.output_micro_usd_per_mtok
                    .max(long.output_micro_usd_per_mtok),
            ),
            Some(_) | None => flat,
        }
    }

    /// What a request with `input_tokens` of prompt and `output_tokens` of output costs, at the
    /// rate its prompt pays, rounded up.
    pub fn cost(&self, input_tokens: u64, output_tokens: u64) -> u64 {
        let rate = self.for_prompt(input_tokens);
        rate.input_cost(input_tokens)
            .saturating_add(rate.output_cost(output_tokens))
    }

    /// What `tokens` input tokens cost at the short rate, rounded up. For a whole request, use
    /// [`Self::cost`] or [`Self::for_prompt`].
    pub fn input_cost(&self, tokens: u64) -> u64 {
        cost(tokens, self.input_micro_usd_per_mtok)
    }

    /// What `tokens` output tokens cost at the short rate, rounded up.
    pub fn output_cost(&self, tokens: u64) -> u64 {
        cost(tokens, self.output_micro_usd_per_mtok)
    }
}

fn cost(tokens: u64, per_mtok: u64) -> u64 {
    let micro = (u128::from(tokens) * u128::from(per_mtok)).div_ceil(1_000_000);
    u64::try_from(micro).unwrap_or(u64::MAX)
}

/// What a request will cost at most, known before it's sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestEstimate {
    /// The model's price.
    pub price: Price,
    /// An upper bound on input tokens (see [`input_token_bound`]).
    pub input_tokens: u64,
    /// Per-call vendor tool fees for this request (web search and the like), at most.
    pub tool_fees_micro_usd: u64,
    /// The most output tokens the adapter would ask for with no budget in the way.
    pub max_output_tokens: u32,
}

/// An admitted request: send it with this output limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Admission {
    /// The `max_output_tokens` the request must carry.
    pub max_output_tokens: u32,
    /// The most this request can cost, held against the cap until its usage is reported.
    pub worst_case_micro_usd: u64,
}

/// Budget settings for runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetConfig {
    /// The hard cap for one run (security model defaults: $1).
    pub per_run_cap_micro_usd: u64,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            per_run_cap_micro_usd: MICRO_USD_PER_USD,
        }
    }
}

/// Why the meter refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BudgetError {
    /// The request's worst case would cross the cap.
    #[error(
        "This run would go over its {} spend cap ({} used so far). Raise the cap or start a new task.",
        usd(*cap_micro_usd),
        usd(*committed_micro_usd)
    )]
    OverCap {
        /// The run's cap.
        cap_micro_usd: u64,
        /// Billed plus held for requests in flight.
        committed_micro_usd: u64,
        /// The least this request could cost (input, fees and the smallest answer).
        needed_micro_usd: u64,
    },
    /// The run reached its soft limit: the next request waits until the user approves going on
    /// (a `budget.threshold` checkpoint), which lifts the limit to the hard cap.
    #[error(
        "This run reached its {} soft limit ({} used so far). Approve to let it go on, up to its {} cap.",
        usd(*limit_micro_usd),
        usd(*committed_micro_usd),
        usd(*cap_micro_usd)
    )]
    SoftLimit {
        /// The soft limit.
        limit_micro_usd: u64,
        /// Billed plus held for requests in flight.
        committed_micro_usd: u64,
        /// The hard cap approval would lift it to.
        cap_micro_usd: u64,
    },
    /// The request's worst case would cross a monthly cap.
    #[error(
        "{} would go over {} {} monthly spend cap ({} used this month). Raise the cap or wait until next month.",
        match cap { MonthlyCap::Agent => "This agent", MonthlyCap::All => "Your agents together" },
        match cap { MonthlyCap::Agent => "its", MonthlyCap::All => "the" },
        usd(*cap_micro_usd),
        usd(*committed_micro_usd)
    )]
    OverMonth {
        /// Which cap.
        cap: MonthlyCap,
        /// The cap.
        cap_micro_usd: u64,
        /// Billed this month plus held for requests in flight.
        committed_micro_usd: u64,
        /// What the request needs at least: its input, tool fees and the smallest answer.
        needed_micro_usd: u64,
    },
    /// Something tried to log `model.request` without going through the meter.
    #[error("a model request must be admitted by the budget meter first")]
    NotAdmitted,
}

/// Dollars for a message: cents when exact, else four decimals, rounded up.
pub fn usd(micro_usd: u64) -> String {
    let dollars = micro_usd / MICRO_USD_PER_USD;
    let rest = micro_usd % MICRO_USD_PER_USD;
    if rest.is_multiple_of(10_000) {
        format!("${dollars}.{:02}", rest / 10_000)
    } else {
        let ten_thousandths = rest.div_ceil(100);
        if ten_thousandths == 10_000 {
            format!("${}.0000", dollars + 1)
        } else {
            format!("${dollars}.{ten_thousandths:04}")
        }
    }
}

/// One admitted request waiting for its usage.
/// A request's usage, settled: the hold it gave back and what it was charged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settled {
    /// The worst case that was held for it (0 if nothing was).
    pub held_micro_usd: u64,
    /// What it was charged: the billed cost, or tokens × price if that's more.
    pub charged_micro_usd: u64,
}

/// The tightest room outside a run: a monthly cap, from the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Outer {
    pub(crate) room_micro_usd: u64,
    pub(crate) cap: MonthlyCap,
    pub(crate) cap_micro_usd: u64,
    pub(crate) committed_micro_usd: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Reservation {
    worst_case_micro_usd: u64,
    price: Price,
    tool_fees_micro_usd: u64,
}

/// One run's spend against its hard cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunBudget {
    cap_micro_usd: u64,
    spent_micro_usd: u64,
    reservations: VecDeque<Reservation>,
    /// Below the cap, or none.
    soft_micro_usd: Option<u64>,
    /// The user approved going past the soft limit.
    past_soft: bool,
}

impl RunBudget {
    /// A run with nothing spent and this cap.
    pub fn new(cap_micro_usd: u64) -> Self {
        Self {
            cap_micro_usd,
            spent_micro_usd: 0,
            reservations: VecDeque::new(),
            soft_micro_usd: None,
            past_soft: false,
        }
    }

    /// The same budget with a soft limit (TP-BUD-02). One at or over the cap is no soft limit.
    pub fn with_soft_limit(mut self, soft_micro_usd: u64) -> Self {
        self.soft_micro_usd = (soft_micro_usd < self.cap_micro_usd).then_some(soft_micro_usd);
        self
    }

    /// The soft limit, if the run has one.
    pub fn soft_limit(&self) -> Option<u64> {
        self.soft_micro_usd
    }

    /// Whether the user approved going past the soft limit.
    pub fn past_soft_limit(&self) -> bool {
        self.past_soft
    }

    /// The user approved going on: for the rest of the run, only the hard cap applies.
    pub fn approve_soft_limit(&mut self) {
        self.past_soft = true;
    }

    /// The limit requests are admitted under now: the soft limit until it's approved, then the
    /// cap.
    fn limit(&self) -> u64 {
        match self.soft_micro_usd {
            Some(soft) if !self.past_soft => soft,
            Some(_) | None => self.cap_micro_usd,
        }
    }

    /// The cap.
    pub fn cap(&self) -> u64 {
        self.cap_micro_usd
    }

    /// Billed so far.
    pub fn spent(&self) -> u64 {
        self.spent_micro_usd
    }

    /// Billed so far plus the worst case of admitted requests not yet billed.
    pub fn committed(&self) -> u64 {
        self.reservations
            .iter()
            .fold(self.spent_micro_usd, |sum, r| {
                sum.saturating_add(r.worst_case_micro_usd)
            })
    }

    /// Admits a request if its worst case fits under the limit (the soft limit until approved,
    /// then the cap), holding that worst case until its usage is reported. A refusal holds
    /// nothing.
    pub fn admit(&mut self, estimate: &RequestEstimate) -> Result<Admission, BudgetError> {
        self.admit_under(estimate, None)
    }

    /// [`Self::admit`], also under `outer`: the tightest monthly cap's room. The run's own limits
    /// are checked first.
    pub(crate) fn admit_under(
        &mut self,
        estimate: &RequestEstimate,
        outer: Option<Outer>,
    ) -> Result<Admission, BudgetError> {
        // The rate the input bound pays: the bound is never below the real prompt, so a request
        // that comes in shorter is billed at this rate or less.
        let price = estimate.price.for_prompt(estimate.input_tokens);
        let committed = self.committed();
        let limit = self.limit();
        let run_remaining = limit.saturating_sub(committed);
        let fixed = price
            .input_cost(estimate.input_tokens)
            .saturating_add(estimate.tool_fees_micro_usd);
        let needed = fixed.saturating_add(price.output_cost(u64::from(MIN_OUTPUT_TOKENS)));
        if needed > run_remaining && limit < self.cap_micro_usd {
            return Err(BudgetError::SoftLimit {
                limit_micro_usd: limit,
                committed_micro_usd: committed,
                cap_micro_usd: self.cap_micro_usd,
            });
        }
        if needed > run_remaining {
            return Err(BudgetError::OverCap {
                cap_micro_usd: self.cap_micro_usd,
                committed_micro_usd: committed,
                needed_micro_usd: needed,
            });
        }
        let remaining = match outer {
            Some(outer) if needed > outer.room_micro_usd => {
                return Err(BudgetError::OverMonth {
                    cap: outer.cap,
                    cap_micro_usd: outer.cap_micro_usd,
                    committed_micro_usd: outer.committed_micro_usd,
                    needed_micro_usd: needed,
                });
            }
            Some(outer) => run_remaining.min(outer.room_micro_usd),
            None => run_remaining,
        };
        let ceiling = u64::from(estimate.max_output_tokens);
        let per_mtok = price.output_micro_usd_per_mtok;
        let affordable = if per_mtok == 0 {
            ceiling
        } else {
            // The largest n with n × price ≤ room × 10^6, so its rounded-up cost still fits.
            let room = u128::from(remaining - fixed) * 1_000_000;
            u64::try_from(room / u128::from(per_mtok)).unwrap_or(u64::MAX)
        };
        let max_output = ceiling.min(affordable);
        let worst_case = fixed.saturating_add(price.output_cost(max_output));
        self.reservations.push_back(Reservation {
            worst_case_micro_usd: worst_case,
            price: estimate.price,
            tool_fees_micro_usd: estimate.tool_fees_micro_usd,
        });
        Ok(Admission {
            // max_output ≤ ceiling, which came from a u32.
            max_output_tokens: u32::try_from(max_output).unwrap_or(estimate.max_output_tokens),
            worst_case_micro_usd: worst_case,
        })
    }

    /// Gives back the newest reservation: its request was admitted but never sent.
    pub(crate) fn release_last(&mut self) {
        self.reservations.pop_back();
    }

    /// Counts a request's billed usage, releasing the oldest reservation. A cost below what the
    /// tokens come to at the admitted price is charged at that price instead.
    pub fn reconcile(&mut self, usage: &UsageReported) -> Settled {
        let reservation = self.reservations.pop_front();
        let floor = reservation.map_or(0, |r| {
            r.price
                .cost(usage.input_tokens, usage.output_tokens)
                .saturating_add(r.tool_fees_micro_usd)
        });
        let charged = usage.cost_micro_usd.max(floor);
        self.spent_micro_usd = self.spent_micro_usd.saturating_add(charged);
        Settled {
            held_micro_usd: reservation.map_or(0, |r| r.worst_case_micro_usd),
            charged_micro_usd: charged,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dollars_read_plainly() {
        assert_eq!(usd(1_000_000), "$1.00");
        assert_eq!(usd(10_000), "$0.01");
        assert_eq!(usd(0), "$0.00");
        assert_eq!(usd(306), "$0.0004");
        assert_eq!(usd(1_999_999), "$2.0000");
        assert_eq!(usd(1_234_500), "$1.2345");
    }

    #[test]
    fn costs_round_up() {
        let price = Price::per_million_tokens(2_000_000, 6_000_000);
        assert_eq!(price.input_cost(1), 2);
        assert_eq!(Price::per_million_tokens(1, 1).input_cost(1), 1);
        assert_eq!(price.output_cost(u64::MAX), u64::MAX, "saturates");
    }
}
