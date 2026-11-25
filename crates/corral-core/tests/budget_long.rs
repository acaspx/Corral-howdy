//! Long-prompt prices (TP-BUD-08; security model T25). Some models cost more per token once a
//! request's prompt is long enough: Grok 4.7 doubles from 200,000 prompt tokens, Claude Haiku
//! 5.5 costs five times as much past 100,000. The higher rate applies to every token of that
//! request, output included. The meter picks the rate from the request's input bound, which is
//! never below its real prompt, so a worst case never undercounts; a bill is priced from the
//! tokens the provider reports.
//!
//! Prices below: $2 / $6 per million tokens, and $4 / $12 from 1,000 prompt tokens.

use corral_core::budget::{LongPrompt, Price, RequestEstimate, RunBudget};
use corral_core::event::UsageReported;
use proptest::prelude::*;

const FROM: u64 = 1_000;
const PRICE: Price =
    Price::per_million_tokens(2_000_000, 6_000_000).with_long_prompt(FROM, 4_000_000, 12_000_000);

fn estimate(input_tokens: u64, max_output_tokens: u32) -> RequestEstimate {
    RequestEstimate {
        price: PRICE,
        input_tokens,
        tool_fees_micro_usd: 0,
        max_output_tokens,
    }
}

fn usage(input_tokens: u64, output_tokens: u64, cost_micro_usd: u64) -> UsageReported {
    UsageReported {
        input_tokens,
        output_tokens,
        cost_micro_usd,
    }
}

#[test]
fn tp_bud_08_a_prompt_from_the_threshold_on_pays_the_long_rate() {
    assert_eq!(
        PRICE.long_prompt,
        Some(LongPrompt {
            from_input_tokens: FROM,
            input_micro_usd_per_mtok: 4_000_000,
            output_micro_usd_per_mtok: 12_000_000,
        })
    );
    let short = Price::per_million_tokens(2_000_000, 6_000_000);
    let long = Price::per_million_tokens(4_000_000, 12_000_000);
    assert_eq!(PRICE.for_prompt(FROM - 1), short);
    assert_eq!(PRICE.for_prompt(FROM), long, "the threshold itself is long");
    assert_eq!(PRICE.for_prompt(u64::MAX), long);
    assert_eq!(PRICE.cost(FROM - 1, 1_000), 1_998 + 6_000);
    assert_eq!(PRICE.cost(FROM, 1_000), 4_000 + 12_000, "output too");
    assert_eq!(short.for_prompt(u64::MAX), short, "a flat price stays flat");
    assert_eq!(short.cost(1, 1), 2 + 6);
}

#[test]
fn tp_bud_08_a_long_rate_never_lowers_a_price() {
    let odd = Price::per_million_tokens(2_000_000, 6_000_000).with_long_prompt(10, 1, 9_000_000);
    assert_eq!(
        odd.for_prompt(10),
        Price::per_million_tokens(2_000_000, 9_000_000),
        "each rate is the higher of the two"
    );
}

#[test]
fn tp_bud_08_the_worst_case_is_held_at_the_rate_its_input_bound_pays() {
    let mut budget = RunBudget::new(1_000_000);
    let below = budget.admit(&estimate(FROM - 1, 1_000)).expect("admitted");
    assert_eq!(below.worst_case_micro_usd, 1_998 + 6_000);
    let at = budget.admit(&estimate(FROM, 1_000)).expect("admitted");
    assert_eq!(at.worst_case_micro_usd, 4_000 + 12_000);
}

#[test]
fn tp_bud_08_the_output_limit_comes_from_the_long_rate() {
    // 4,000 µ$ of input at the long rate leaves 6,000 µ$: 500 output tokens at 12 µ$, not 1,000
    // at 6 µ$.
    let mut budget = RunBudget::new(10_000);
    let admitted = budget.admit(&estimate(FROM, 16_384)).expect("admitted");
    assert_eq!(admitted.max_output_tokens, 500);
    assert_eq!(admitted.worst_case_micro_usd, 10_000);
}

#[test]
fn tp_bud_08_a_long_bill_below_its_tokens_is_charged_at_the_long_rate() {
    let mut budget = RunBudget::new(1_000_000);
    budget.admit(&estimate(2_000, 1_000)).expect("admitted");
    let settled = budget.reconcile(&usage(FROM, 1_000, 1));
    assert_eq!(settled.charged_micro_usd, 4_000 + 12_000);
    budget.admit(&estimate(2_000, 1_000)).expect("admitted");
    let settled = budget.reconcile(&usage(FROM - 1, 1_000, 1));
    assert_eq!(
        settled.charged_micro_usd,
        1_998 + 6_000,
        "a prompt that came in short is billed short"
    );
}

proptest! {
    /// Whatever the prompt turns out to be, at most its input bound, the bill at the reported
    /// tokens never exceeds the worst case held for it.
    #[test]
    fn tp_bud_08_the_held_worst_case_covers_the_bill(
        bound in 0u64..5_000,
        prompt_cut in 0u64..5_000,
        output in 0u64..20_000,
    ) {
        let mut budget = RunBudget::new(1_000_000);
        let admitted = budget.admit(&estimate(bound, 16_384)).expect("admitted");
        let prompt = bound.saturating_sub(prompt_cut);
        let output = output.min(u64::from(admitted.max_output_tokens));
        let settled = budget.reconcile(&usage(prompt, output, 0));
        prop_assert!(settled.charged_micro_usd <= admitted.worst_case_micro_usd);
    }
}
