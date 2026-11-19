//! Cost scenarios with several runs at once (M2 item 8, EV-COST-01 and 03; TP-EVAL-14..15):
//! `runs` starts that many copies of the scenario together, spread over `agents` round robin,
//! under the scenario's `budget` (a price and caps). `expect.spend` is graded from the log: what
//! each run, each agent and all of them were billed (the reported cost, or tokens × price if
//! that's more) must stay under the caps it names, and at least `refused_at_least` runs must have
//! been stopped by a cap, so a scenario can't pass because the caps never bound.
//!
//! The fake asks for `prompt.len() + 256` input tokens and up to 1,024 output tokens per turn. At
//! $2 and $6 per million tokens, "Summarize the news." (19 bytes) is 275 input tokens (550 µ$) and
//! a worst case of 6,694 µ$; reporting 500 output tokens bills 3,550 µ$.

use corral_core::event::{EventBody, FailureReason};
use corral_evals::{Outcome, Profiles, Status, execute_all, parse_scenario};
use std::path::Path;

fn one(text: &str) -> Outcome {
    let scenario = parse_scenario(Path::new("x.yaml"), text).expect("valid scenario");
    execute_all(&[scenario], &Profiles::builtin()).remove(0)
}

fn reason(outcome: &Outcome) -> String {
    outcome.reason.clone().unwrap_or_default()
}

fn refused(outcome: &Outcome) -> usize {
    outcome
        .events
        .iter()
        .filter(|e| {
            matches!(&e.body, EventBody::RunFailed(f) if f.reason == FailureReason::BudgetHardCap)
        })
        .count()
}

fn billed(outcome: &Outcome) -> u64 {
    outcome
        .events
        .iter()
        .filter_map(|e| {
            let EventBody::UsageReported(u) = &e.body else {
                return None;
            };
            Some(u.cost_micro_usd)
        })
        .sum()
}

/// Six runs of one agent at once, held to a $0.02 month: three fit (the third with a shorter
/// answer), three are refused before anything is sent.
const RACE: &str = r#"
id: EV-COST-91
title: Six runs race one agent's monthly cap
agent: { adapter: fake, policy_profile: default }
prompt: "Summarize the news."
budget:
  price_per_million_tokens: { input_micro_usd: 2000000, output_micro_usd: 6000000 }
  agent_month_cap_micro_usd: 20000
runs: 6
steps:
  - model_says: "Here's the summary."
  - wait_ms: 1000
  - usage: { input_tokens: 275, output_tokens: 500 }
expect:
  spend:
    never_over: [run, agent_month, all_month]
    refused_at_least: 3
"#;

#[test]
fn tp_eval_14_runs_racing_a_cap_pass_when_the_cap_holds() {
    let outcome = one(RACE);
    assert_eq!(
        outcome.result.status,
        Status::Passed,
        "{}",
        reason(&outcome)
    );
    assert_eq!(refused(&outcome), 3);
    assert_eq!(billed(&outcome), 3 * 3_550, "three runs billed");
    let runs: std::collections::BTreeSet<String> = outcome
        .events
        .iter()
        .map(|e| e.run_id.to_string())
        .collect();
    assert_eq!(runs.len(), 6, "{runs:?}");
}

#[test]
fn tp_eval_14_runs_are_spread_over_agents_round_robin() {
    // Three agents, two runs each, each agent capped at one run's worst case: nobody is refused.
    let text = RACE
        .replace(
            "agent_month_cap_micro_usd: 20000",
            "agent_month_cap_micro_usd: 13388",
        )
        .replace("runs: 6", "runs: 6\nagents: 3")
        .replace("refused_at_least: 3", "refused_at_least: 0");
    let outcome = one(&text);
    assert_eq!(
        outcome.result.status,
        Status::Passed,
        "{}",
        reason(&outcome)
    );
    assert_eq!(refused(&outcome), 0);
    let agents: std::collections::BTreeSet<String> = outcome
        .events
        .iter()
        .filter_map(|e| {
            let EventBody::RunQueued(q) = &e.body else {
                return None;
            };
            Some(q.agent_id.to_string())
        })
        .collect();
    assert_eq!(agents.len(), 3, "{agents:?}");
    // Round robin: run 1 and run 4 belong to agent 1, run 2 to agent 2, run 6 to agent 3.
    let agent_of = |run: &str| {
        outcome
            .events
            .iter()
            .find_map(|e| {
                let EventBody::RunQueued(q) = &e.body else {
                    return None;
                };
                (e.run_id.to_string() == run).then(|| q.agent_id.to_string())
            })
            .expect("queued")
    };
    assert_eq!(agent_of("eval-run-1"), "eval-agent-1");
    assert_eq!(agent_of("eval-run-2"), "eval-agent-2");
    assert_eq!(agent_of("eval-run-4"), "eval-agent-1");
    assert_eq!(agent_of("eval-run-6"), "eval-agent-3");
}

#[test]
fn tp_eval_14_the_cap_for_all_agents_binds_across_agents() {
    // Four agents, eight runs, $0.02 for all of them together.
    let text = RACE
        .replace(
            "agent_month_cap_micro_usd: 20000",
            "global_month_cap_micro_usd: 20000",
        )
        .replace("runs: 6", "runs: 8\nagents: 4")
        .replace("refused_at_least: 3", "refused_at_least: 5");
    let outcome = one(&text);
    assert_eq!(
        outcome.result.status,
        Status::Passed,
        "{}",
        reason(&outcome)
    );
    assert_eq!(refused(&outcome), 5);
}

#[test]
fn tp_eval_14_billing_past_a_cap_fails_and_says_by_how_much() {
    // One run reports $0.10 for a turn whose worst case was $0.0067: the bill is what the
    // provider reported, so the month is over its cap and the scenario fails.
    let text = RACE
        .replace("runs: 6\n", "")
        .replace(
            "usage: { input_tokens: 275, output_tokens: 500 }",
            "usage: { input_tokens: 275, output_tokens: 500, cost_micro_usd: 100000 }",
        )
        .replace("refused_at_least: 3", "refused_at_least: 0");
    let outcome = one(&text);
    assert_eq!(outcome.result.status, Status::Failed);
    let why = reason(&outcome);
    assert!(
        why.contains("eval-agent-1 was billed $0.10 this month; its cap is $0.02"),
        "{why}"
    );
    assert!(
        !why.contains("all agents were billed"),
        "the global cap ($100) held: {why}"
    );
}

#[test]
fn tp_eval_14_a_scenario_where_no_cap_bound_fails() {
    let text = RACE
        .replace(
            "agent_month_cap_micro_usd: 20000",
            "agent_month_cap_micro_usd: 25000000",
        )
        .replace("refused_at_least: 3", "refused_at_least: 1");
    let outcome = one(&text);
    assert_eq!(outcome.result.status, Status::Failed);
    assert!(
        reason(&outcome).contains("expected a cap to refuse at least 1 run; 0 were refused"),
        "{}",
        reason(&outcome)
    );
}

#[test]
fn tp_eval_14_usage_without_a_cost_is_billed_at_tokens_times_price() {
    let text = RACE
        .replace("runs: 6\n", "")
        .replace("refused_at_least: 3", "refused_at_least: 0");
    let outcome = one(&text);
    assert_eq!(
        outcome.result.status,
        Status::Passed,
        "{}",
        reason(&outcome)
    );
    assert_eq!(billed(&outcome), 3_550);
}

#[test]
fn tp_eval_14_usage_without_a_cost_stops_at_the_requests_output_limit() {
    // The model would write 5,000 tokens; the request allowed 1,024, so that's what's billed.
    let text = RACE
        .replace("runs: 6\n", "")
        .replace("output_tokens: 500 }", "output_tokens: 5000 }")
        .replace("refused_at_least: 3", "refused_at_least: 0");
    let outcome = one(&text);
    assert_eq!(
        outcome.result.status,
        Status::Passed,
        "{}",
        reason(&outcome)
    );
    let reported: Vec<(u64, u64)> = outcome
        .events
        .iter()
        .filter_map(|e| {
            let EventBody::UsageReported(u) = &e.body else {
                return None;
            };
            Some((u.output_tokens, u.cost_micro_usd))
        })
        .collect();
    assert_eq!(reported, [(1_024, 550 + 1_024 * 6)]);
}

#[test]
fn tp_eval_14_a_new_turn_asks_the_budget_again() {
    // Two turns of one run under a $0.004 run cap: the first turn fits with a shorter answer
    // (575 tokens, 4,000 µ$) and bills 3,550 µ$; the 450 µ$ left can't cover the second.
    let text = RACE
        .replace("runs: 6\n", "")
        .replace("  agent_month_cap_micro_usd: 20000\n", "  run_cap_micro_usd: 4000\n")
        .replace(
            "  - usage: { input_tokens: 275, output_tokens: 500 }",
            "  - usage: { input_tokens: 275, output_tokens: 500 }\n  - new_turn: true\n  - usage: { input_tokens: 275, output_tokens: 500 }",
        )
        .replace("refused_at_least: 3", "refused_at_least: 1");
    let outcome = one(&text);
    assert_eq!(
        outcome.result.status,
        Status::Passed,
        "{}",
        reason(&outcome)
    );
    assert_eq!(billed(&outcome), 3_550, "only the first turn was billed");
}

// ---- TP-EVAL-15: the schema for these ------------------------------------------------------------

fn invalid(text: &str) -> String {
    parse_scenario(Path::new("x.yaml"), text)
        .expect_err("invalid")
        .to_string()
}

#[test]
fn tp_eval_15_runs_and_agents_must_make_sense() {
    assert!(invalid(&RACE.replace("runs: 6", "runs: 0")).contains("`runs` must be from 1 to 32"));
    assert!(invalid(&RACE.replace("runs: 6", "runs: 33")).contains("`runs` must be from 1 to 32"));
    assert!(
        invalid(&RACE.replace("runs: 6", "runs: 6\nagents: 7"))
            .contains("`agents` must be from 1 to `runs` (6)")
    );
    assert!(
        invalid(&RACE.replace("runs: 6", "runs: 6\nagents: 0"))
            .contains("`agents` must be from 1 to `runs` (6)")
    );
}

#[test]
fn tp_eval_15_checkpoints_are_judged_in_one_run_only() {
    let text = RACE.replace(
        "expect:\n",
        "expect:\n  checkpoint: { class: comms.send }\n",
    );
    assert!(
        invalid(&text).contains(
            "`checkpoint` and `if_denied` judge one run; with `runs` over 1, use `spend` or `never`"
        ),
        "{}",
        invalid(&text)
    );
}

#[test]
fn tp_eval_15_each_step_does_one_thing() {
    let two = RACE.replace("  - wait_ms: 1000", "  - { wait_ms: 1000, new_turn: true }");
    assert!(
        invalid(&two).contains(
            "step 2: set exactly one of `tool`, `model_says`, `usage`, `new_turn` or `wait_ms`"
        ),
        "{}",
        invalid(&two)
    );
    let off = RACE.replace("  - wait_ms: 1000", "  - new_turn: false");
    assert!(invalid(&off).contains("step 2: `new_turn` can only be true"));
    let args = RACE.replace("  - wait_ms: 1000", "  - { wait_ms: 1000, args: { a: 1 } }");
    assert!(
        invalid(&args).contains("step 2: `args` and `returns_fixture` belong to a `tool` step")
    );
}

#[test]
fn tp_eval_15_budget_and_spend_reject_unknown_keys_and_empty_expectations() {
    assert!(
        invalid(&RACE.replace("agent_month_cap_micro_usd", "agent_cap")).contains("unknown field")
    );
    assert!(
        invalid(&RACE.replace(
            "never_over: [run, agent_month, all_month]",
            "never_over: [monthly]"
        ))
        .contains("unknown variant")
    );
    let empty = RACE.replace(
        "    never_over: [run, agent_month, all_month]\n    refused_at_least: 3\n",
        "    never_over: []\n",
    );
    assert!(
        invalid(&empty).contains("`spend` needs `never_over` or `refused_at_least`"),
        "{}",
        invalid(&empty)
    );
}

#[test]
fn tp_eval_15_only_spend_makes_an_expectation_too() {
    let only = RACE.replace("    refused_at_least: 3\n", "");
    assert!(parse_scenario(Path::new("x.yaml"), &only).is_ok());
}

#[test]
fn tp_eval_14_the_shipped_cost_scenarios_bind_their_caps() {
    // Both fill their $0.03 cap exactly and stop the runs that don't fit: two of six in
    // EV-COST-01, four of eight in EV-COST-03.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals/scenarios");
    for (file, refusals) in [("cost-race.yaml", 2), ("cost-fan-out.yaml", 4)] {
        let path = dir.join(file);
        let text = std::fs::read_to_string(&path).expect("scenario");
        let scenario = parse_scenario(&path, &text).expect("valid");
        let outcome = execute_all(&[scenario], &Profiles::builtin()).remove(0);
        assert_eq!(
            outcome.result.status,
            Status::Passed,
            "{file}: {}",
            reason(&outcome)
        );
        assert_eq!(refused(&outcome), refusals, "{file}");
        assert_eq!(billed(&outcome), 30_000, "{file}");
    }
}
