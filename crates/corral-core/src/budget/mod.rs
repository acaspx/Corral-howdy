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
//! Money is in micro-USD (10^-6 USD), like `usage.reported`. Soft limits, the monthly and global
//! caps and the versioned price table come later in M2 (TP-BUD-02..04).

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
}

impl Price {
    /// A price in micro-USD per million tokens: `$2` per million is `2_000_000`.
    pub const fn per_million_tokens(input: u64, output: u64) -> Self {
        Self {
            input_micro_usd_per_mtok: input,
            output_micro_usd_per_mtok: output,
        }
    }

    /// What `tokens` input tokens cost, rounded up.
    pub fn input_cost(&self, tokens: u64) -> u64 {
        cost(tokens, self.input_micro_usd_per_mtok)
    }

    /// What `tokens` output tokens cost, rounded up.
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
}

impl RunBudget {
    /// A run with nothing spent and this cap.
    pub fn new(cap_micro_usd: u64) -> Self {
        Self {
            cap_micro_usd,
            spent_micro_usd: 0,
            reservations: VecDeque::new(),
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

    /// Admits a request if its worst case fits under the cap, holding that worst case until its
    /// usage is reported. A refusal holds nothing.
    pub fn admit(&mut self, estimate: &RequestEstimate) -> Result<Admission, BudgetError> {
        let price = estimate.price;
        let committed = self.committed();
        let remaining = self.cap_micro_usd.saturating_sub(committed);
        let fixed = price
            .input_cost(estimate.input_tokens)
            .saturating_add(estimate.tool_fees_micro_usd);
        let needed = fixed.saturating_add(price.output_cost(u64::from(MIN_OUTPUT_TOKENS)));
        if needed > remaining {
            return Err(BudgetError::OverCap {
                cap_micro_usd: self.cap_micro_usd,
                committed_micro_usd: committed,
                needed_micro_usd: needed,
            });
        }
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
            price,
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
    pub fn reconcile(&mut self, usage: &UsageReported) {
        let floor = self.reservations.pop_front().map_or(0, |r| {
            r.price
                .input_cost(usage.input_tokens)
                .saturating_add(r.price.output_cost(usage.output_tokens))
                .saturating_add(r.tool_fees_micro_usd)
        });
        self.spent_micro_usd = self
            .spent_micro_usd
            .saturating_add(usage.cost_micro_usd.max(floor));
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
