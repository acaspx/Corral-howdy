//! Each agent's own monthly cap (M4 item 6; TP-AED-01; security model T9). The agent editor
//! sets an agent's cap from $0 up to the cap for all agents together, which still holds for
//! everyone; an agent without one has the default ($25). A cap above the all-agents cap is
//! refused and changes nothing. A cap takes effect at the agent's next request, and a cap lowered
//! below what's already committed this month refuses the next request.
//!
//! Prices are $2 per million input tokens and $6 per million output tokens.

use corral_core::budget::{
    BudgetError, CapError, LedgerConfig, LocalMonths, MonthlyCap, Price, RequestEstimate,
    RunBudget, Settled, SpendLedger,
};
use corral_core::event::UsageReported;
use corral_core::ids::AgentId;

const PRICE: Price = Price::per_million_tokens(2_000_000, 6_000_000);
/// 2026-11-15 12:00 UTC, in ms.
const NOW: u64 = 1_794_744_000_000;

fn ledger() -> SpendLedger {
    SpendLedger::new(
        LedgerConfig {
            agent_month_cap_micro_usd: 25_000,
            global_month_cap_micro_usd: 100_000,
        },
        LocalMonths::fixed(-8).expect("an offset"),
    )
}

/// 1,000 µ$ of input and room for 1,000 µ$ of output at most.
fn estimate() -> RequestEstimate {
    RequestEstimate {
        price: PRICE,
        input_tokens: 500,
        tool_fees_micro_usd: 0,
        max_output_tokens: 166,
    }
}

fn grok() -> AgentId {
    AgentId::from("grok")
}

fn claude() -> AgentId {
    AgentId::from("claude")
}

/// Spends `micro_usd` for `agent` this month.
fn spend(ledger: &SpendLedger, agent: &AgentId, micro_usd: u64) {
    ledger.seed(agent, NOW, micro_usd);
}

fn admit(ledger: &SpendLedger, agent: &AgentId) -> Result<u64, BudgetError> {
    let mut run = RunBudget::new(1_000_000_000);
    ledger
        .admit(agent, NOW, &mut run, &estimate())
        .map(|a| a.worst_case_micro_usd)
}

#[test]
fn tp_aed_01_an_agent_without_its_own_cap_has_the_default() {
    let ledger = ledger();
    assert_eq!(ledger.agent_cap(&grok()), 25_000);
    spend(&ledger, &grok(), 24_500);
    assert!(
        matches!(
            admit(&ledger, &grok()),
            Err(BudgetError::OverMonth {
                cap: MonthlyCap::Agent,
                cap_micro_usd: 25_000,
                ..
            })
        ),
        "the default still holds"
    );
}

#[test]
fn tp_aed_01_an_agents_own_cap_holds_for_it_alone() {
    let ledger = ledger();
    ledger.set_agent_cap(&grok(), Some(3_000)).expect("set");
    assert_eq!(ledger.agent_cap(&grok()), 3_000);
    assert_eq!(
        ledger.agent_cap(&claude()),
        25_000,
        "the others keep the default"
    );
    spend(&ledger, &grok(), 1_000);
    assert_eq!(
        admit(&ledger, &grok()),
        Ok(1_996),
        "the output limit comes from its room"
    );
    let refused = admit(&ledger, &grok());
    assert_eq!(
        refused,
        Err(BudgetError::OverMonth {
            cap: MonthlyCap::Agent,
            cap_micro_usd: 3_000,
            committed_micro_usd: 2_996,
            needed_micro_usd: 1_096,
        })
    );
    assert!(admit(&ledger, &claude()).is_ok());
}

#[test]
fn tp_aed_01_a_cap_can_go_up_to_the_all_agents_cap_and_no_further() {
    let ledger = ledger();
    ledger
        .set_agent_cap(&grok(), Some(100_000))
        .expect("up to the all-agents cap");
    assert_eq!(ledger.agent_cap(&grok()), 100_000);
    let refused = ledger.set_agent_cap(&grok(), Some(100_001));
    assert_eq!(
        refused,
        Err(CapError::AboveAllAgents {
            cap_micro_usd: 100_001,
            all_agents_micro_usd: 100_000,
        })
    );
    assert_eq!(
        ledger.agent_cap(&grok()),
        100_000,
        "a refused cap changes nothing"
    );
    let message = refused.expect_err("refused").to_string();
    assert!(message.contains("$0.10"), "{message}");
    spend(&ledger, &claude(), 99_500);
    assert!(
        matches!(
            admit(&ledger, &grok()),
            Err(BudgetError::OverMonth {
                cap: MonthlyCap::All,
                ..
            })
        ),
        "the all-agents cap still holds for everyone"
    );
}

#[test]
fn tp_aed_01_a_zero_cap_admits_nothing_and_none_goes_back_to_the_default() {
    let ledger = ledger();
    ledger.set_agent_cap(&grok(), Some(0)).expect("set");
    assert!(admit(&ledger, &grok()).is_err());
    ledger.set_agent_cap(&grok(), None).expect("cleared");
    assert_eq!(ledger.agent_cap(&grok()), 25_000);
    assert!(admit(&ledger, &grok()).is_ok());
}

#[test]
fn tp_aed_01_a_cap_lowered_below_this_months_spend_refuses_the_next_request() {
    let ledger = ledger();
    let mut run = RunBudget::new(1_000_000_000);
    let held = ledger
        .admit(&grok(), NOW, &mut run, &estimate())
        .expect("admitted");
    ledger.set_agent_cap(&grok(), Some(1_500)).expect("lowered");
    assert!(admit(&ledger, &grok()).is_err(), "what's held counts");
    ledger.settle(
        &grok(),
        NOW,
        Settled {
            held_micro_usd: held.worst_case_micro_usd,
            charged_micro_usd: run
                .reconcile(&UsageReported {
                    input_tokens: 500,
                    output_tokens: 100,
                    cost_micro_usd: 1_600,
                })
                .charged_micro_usd,
        },
    );
    assert_eq!(
        ledger.spent(&grok(), NOW),
        1_600,
        "a request already sent is billed"
    );
    assert!(admit(&ledger, &grok()).is_err());
}
