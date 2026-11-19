//! The monthly caps (M2 item 5; system design §7; security model T9): TP-BUD-03, TP-BUD-04 and
//! TP-BUD-06..07. One `SpendLedger` is shared by every run. A request is admitted under its run's
//! cap, its agent's monthly cap and the monthly cap for all agents together, in one step, so runs
//! racing each other can't add up past a cap. Months are the user's calendar months.
//!
//! Prices are $2 per million input tokens and $6 per million output tokens: 2 µ$ per input token,
//! 6 µ$ per output token.

use corral_core::budget::{
    Admission, BudgetConfig, BudgetError, LedgerConfig, LocalMonths, Month, MonthlyCap, Price,
    RequestEstimate, RunBudget, SpendLedger,
};
use corral_core::clock::ManualClock;
use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId};
use corral_core::supervisor::{
    CancelToken, EventLog, EventSink, LogError, RunDriver, RunFuture, RunRequest, Supervisor,
    SupervisorConfig,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PRICE: Price = Price::per_million_tokens(2_000_000, 6_000_000);

/// 2026-11-15 12:00 UTC, in ms.
const MID_NOVEMBER: u64 = 1_794_744_000_000;
/// 2026-12-01 07:30 UTC: still 30 November, 23:30, eight hours west of UTC.
const LATE_NOVEMBER_WEST: u64 = 1_796_110_200_000;
/// 2026-12-01 08:00 UTC: 1 December, midnight, eight hours west of UTC.
const DECEMBER_WEST: u64 = 1_796_112_000_000;

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

fn west() -> LocalMonths {
    LocalMonths::fixed(-8).expect("an offset")
}

fn ledger(agent_cap: u64, global_cap: u64) -> SpendLedger {
    SpendLedger::new(
        LedgerConfig {
            agent_month_cap_micro_usd: agent_cap,
            global_month_cap_micro_usd: global_cap,
        },
        west(),
    )
}

fn scout() -> AgentId {
    AgentId::from("scout")
}

fn grok() -> AgentId {
    AgentId::from("grok")
}

/// A run with room to spare, so only the monthly caps bind.
fn roomy() -> RunBudget {
    RunBudget::new(1_000_000_000)
}

// ---- The defaults --------------------------------------------------------------------------------

#[test]
fn tp_bud_06_the_defaults_are_the_security_models() {
    assert_eq!(
        LedgerConfig::default(),
        LedgerConfig {
            agent_month_cap_micro_usd: 25_000_000,
            global_month_cap_micro_usd: 100_000_000,
        }
    );
}

// ---- TP-BUD-06: the caps hold across runs ---------------------------------------------------------

#[test]
fn tp_bud_06_an_agents_runs_together_stay_under_its_monthly_cap() {
    let ledger = ledger(1_000, 1_000_000);
    let (mut a, mut b) = (roomy(), roomy());
    // 100 input tokens (200 µ$) with up to 100 output tokens (600 µ$): 800 µ$ worst case.
    let first = ledger
        .admit(&scout(), MID_NOVEMBER, &mut a, &estimate(100, 100))
        .expect("fits");
    assert_eq!(first.worst_case_micro_usd, 800);
    // Another run of the same agent has 200 µ$ left this month: 100 input tokens leave nothing
    // for the 16-token minimum answer (96 µ$).
    assert_eq!(
        ledger.admit(&scout(), MID_NOVEMBER, &mut b, &estimate(100, 100)),
        Err(BudgetError::OverMonth {
            cap: MonthlyCap::Agent,
            cap_micro_usd: 1_000,
            committed_micro_usd: 800,
            needed_micro_usd: 296,
        })
    );
    assert_eq!(b.committed(), 0, "a refused request reserves nothing");
    // Another agent isn't held to Scout's month.
    assert!(
        ledger
            .admit(&grok(), MID_NOVEMBER, &mut roomy(), &estimate(100, 100))
            .is_ok()
    );
}

#[test]
fn tp_bud_06_all_agents_together_stay_under_the_global_cap() {
    let ledger = ledger(1_000_000, 1_000);
    ledger
        .admit(&scout(), MID_NOVEMBER, &mut roomy(), &estimate(100, 100))
        .expect("fits");
    assert_eq!(
        ledger.admit(&grok(), MID_NOVEMBER, &mut roomy(), &estimate(100, 100)),
        Err(BudgetError::OverMonth {
            cap: MonthlyCap::All,
            cap_micro_usd: 1_000,
            committed_micro_usd: 800,
            needed_micro_usd: 296,
        })
    );
}

#[test]
fn tp_bud_06_the_tightest_cap_sets_the_output_limit() {
    // Scout has 500 µ$ left this month, all agents 700 µ$, the run 1,000 µ$.
    let ledger = ledger(500, 700);
    let mut run = RunBudget::new(1_000);
    // 50 input tokens (100 µ$) leave 400 µ$ under Scout's cap: 66 output tokens (396 µ$).
    assert_eq!(
        ledger.admit(&scout(), MID_NOVEMBER, &mut run, &estimate(50, 16_384)),
        Ok(Admission {
            max_output_tokens: 66,
            worst_case_micro_usd: 496,
        })
    );
    assert_eq!(run.committed(), 496);
    assert_eq!(ledger.committed(&scout(), MID_NOVEMBER), 496);
    assert_eq!(ledger.committed_all(MID_NOVEMBER), 496);
    // Grok is now bound by what's left for all agents: 204 µ$, so 34 output tokens after 0 input.
    let grok_admitted = ledger
        .admit(&grok(), MID_NOVEMBER, &mut roomy(), &estimate(0, 16_384))
        .expect("fits");
    assert_eq!(grok_admitted.max_output_tokens, 34);
    assert_eq!(ledger.committed_all(MID_NOVEMBER), 700);
}

#[test]
fn tp_bud_06_the_run_cap_still_binds_first() {
    let ledger = ledger(1_000_000, 1_000_000);
    let mut run = RunBudget::new(1_000);
    assert_eq!(
        ledger.admit(&scout(), MID_NOVEMBER, &mut run, &estimate(453, 16_384)),
        Err(BudgetError::OverCap {
            cap_micro_usd: 1_000,
            committed_micro_usd: 0,
            needed_micro_usd: 1_002,
        })
    );
    assert_eq!(ledger.committed(&scout(), MID_NOVEMBER), 0, "nothing held");
}

#[test]
fn tp_bud_06_a_worst_case_landing_exactly_on_a_monthly_cap_is_allowed() {
    // 452 input tokens (904 µ$) leave 96 µ$: exactly the 16-token answer.
    let ledger = ledger(1_000, 1_000_000);
    let admitted = ledger
        .admit(&scout(), MID_NOVEMBER, &mut roomy(), &estimate(452, 16_384))
        .expect("fits exactly");
    assert_eq!(admitted.worst_case_micro_usd, 1_000);
    assert!(
        ledger
            .admit(&scout(), MID_NOVEMBER, &mut roomy(), &estimate(0, 16_384))
            .is_err()
    );
}

#[test]
fn tp_bud_06_runs_racing_the_same_caps_never_add_up_past_them() {
    let ledger = Arc::new(ledger(50_000, 80_000));
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let ledger = Arc::clone(&ledger);
            let agent = if i % 2 == 0 { scout() } else { grok() };
            std::thread::spawn(move || {
                let mut run = roomy();
                let mut admitted = Vec::new();
                for _ in 0..200 {
                    if let Ok(a) = ledger.admit(&agent, MID_NOVEMBER, &mut run, &estimate(30, 200))
                    {
                        admitted.push((agent.clone(), a.worst_case_micro_usd));
                    }
                }
                admitted
            })
        })
        .collect();
    let admitted: Vec<(AgentId, u64)> = threads
        .into_iter()
        .flat_map(|t| t.join().expect("thread"))
        .collect();
    let total: u64 = admitted.iter().map(|(_, w)| w).sum();
    let scout_total: u64 = admitted
        .iter()
        .filter(|(a, _)| *a == scout())
        .map(|(_, w)| w)
        .sum();
    assert!(total <= 80_000, "all agents: {total}");
    assert!(scout_total <= 50_000, "Scout: {scout_total}");
    assert_eq!(ledger.committed_all(MID_NOVEMBER), total);
    // The caps were reached, not merely approached: under 1,296 µ$ (the largest worst case) is left.
    assert!(80_000 - total < 1_296, "left: {}", 80_000 - total);
}

#[test]
fn tp_bud_06_the_errors_say_which_cap_and_what_to_do() {
    let agent = BudgetError::OverMonth {
        cap: MonthlyCap::Agent,
        cap_micro_usd: 25_000_000,
        committed_micro_usd: 24_990_000,
        needed_micro_usd: 20_000,
    };
    assert_eq!(
        agent.to_string(),
        "This agent would go over its $25.00 monthly spend cap ($24.99 used this month). Raise the cap or wait until next month."
    );
    let all = BudgetError::OverMonth {
        cap: MonthlyCap::All,
        cap_micro_usd: 100_000_000,
        committed_micro_usd: 99_990_000,
        needed_micro_usd: 20_000,
    };
    assert_eq!(
        all.to_string(),
        "Your agents together would go over the $100.00 monthly spend cap ($99.99 used this month). Raise the cap or wait until next month."
    );
}

// ---- TP-BUD-03: reconciliation with provider-reported usage ---------------------------------------

#[test]
fn tp_bud_03_reported_usage_replaces_the_hold_with_the_billed_cost() {
    let ledger = ledger(1_000_000, 1_000_000);
    let mut run = roomy();
    ledger
        .admit(&scout(), MID_NOVEMBER, &mut run, &estimate(100, 100))
        .expect("fits");
    assert_eq!(ledger.committed(&scout(), MID_NOVEMBER), 800);
    let settled = run.reconcile(&usage(100, 10, 300));
    ledger.settle(&scout(), MID_NOVEMBER, settled);
    assert_eq!(ledger.spent(&scout(), MID_NOVEMBER), 300);
    assert_eq!(
        ledger.committed(&scout(), MID_NOVEMBER),
        300,
        "the hold is gone"
    );
    assert_eq!(ledger.committed_all(MID_NOVEMBER), 300);
}

#[test]
fn tp_bud_03_under_reported_cost_is_charged_at_tokens_times_price() {
    let ledger = ledger(1_000_000, 1_000_000);
    let mut run = roomy();
    ledger
        .admit(&scout(), MID_NOVEMBER, &mut run, &estimate(100, 100))
        .expect("fits");
    // 100 input (200 µ$) and 10 output (60 µ$) cost at least 260 µ$, whatever is reported.
    let settled = run.reconcile(&usage(100, 10, 5));
    ledger.settle(&scout(), MID_NOVEMBER, settled);
    assert_eq!(ledger.spent(&scout(), MID_NOVEMBER), 260);
    assert_eq!(run.spent(), 260, "the run and the month agree");
}

#[test]
fn tp_bud_03_a_request_that_was_never_sent_gives_its_hold_back() {
    let ledger = ledger(1_000_000, 1_000_000);
    let admitted = ledger
        .admit(&scout(), MID_NOVEMBER, &mut roomy(), &estimate(100, 100))
        .expect("fits");
    ledger.release(&scout(), admitted.worst_case_micro_usd);
    assert_eq!(ledger.committed(&scout(), MID_NOVEMBER), 0);
    assert_eq!(ledger.committed_all(MID_NOVEMBER), 0);
}

// ---- TP-BUD-04: months are the user's calendar months ---------------------------------------------

#[test]
fn tp_bud_04_the_month_is_the_users_local_month() {
    let months = west();
    assert_eq!(
        months.month(LATE_NOVEMBER_WEST),
        Month {
            year: 2026,
            month: 11
        }
    );
    assert_eq!(
        months.month(DECEMBER_WEST),
        Month {
            year: 2026,
            month: 12
        }
    );
    // In UTC the late-November instant is already December.
    let utc = LocalMonths::fixed(0).expect("UTC");
    assert_eq!(
        utc.month(LATE_NOVEMBER_WEST),
        Month {
            year: 2026,
            month: 12
        }
    );
    assert!(LocalMonths::fixed(30).is_none(), "no such offset");
}

#[test]
fn tp_bud_04_spend_rolls_over_at_local_midnight_on_the_first() {
    let ledger = ledger(1_000, 1_000_000);
    let mut run = roomy();
    ledger
        .admit(&scout(), LATE_NOVEMBER_WEST, &mut run, &estimate(100, 100))
        .expect("fits");
    let settled = run.reconcile(&usage(100, 100, 800));
    ledger.settle(&scout(), LATE_NOVEMBER_WEST, settled);
    assert!(
        ledger
            .admit(
                &scout(),
                LATE_NOVEMBER_WEST,
                &mut roomy(),
                &estimate(100, 100)
            )
            .is_err(),
        "November is spent"
    );
    // Midnight on 1 December, local time: a new month.
    assert_eq!(ledger.spent(&scout(), DECEMBER_WEST), 0);
    assert!(
        ledger
            .admit(&scout(), DECEMBER_WEST, &mut roomy(), &estimate(100, 100))
            .is_ok()
    );
}

#[test]
fn tp_bud_04_a_request_in_flight_at_midnight_is_still_held() {
    let ledger = ledger(1_000, 1_000_000);
    let mut run = roomy();
    ledger
        .admit(&scout(), LATE_NOVEMBER_WEST, &mut run, &estimate(100, 100))
        .expect("fits");
    // It hasn't reported usage yet, so it still counts in December.
    assert_eq!(ledger.committed(&scout(), DECEMBER_WEST), 800);
    let settled = run.reconcile(&usage(100, 100, 800));
    ledger.settle(&scout(), DECEMBER_WEST, settled);
    assert_eq!(
        ledger.spent(&scout(), DECEMBER_WEST),
        800,
        "billed in December"
    );
}

#[test]
fn tp_bud_04_a_clock_that_goes_back_doesnt_reopen_an_old_month() {
    let ledger = ledger(1_000, 1_000_000);
    ledger.seed(&scout(), DECEMBER_WEST, 900);
    // An event stamped in November after December began counts in December, not a fresh month.
    assert_eq!(ledger.spent(&scout(), LATE_NOVEMBER_WEST), 900);
    assert!(
        ledger
            .admit(
                &scout(),
                LATE_NOVEMBER_WEST,
                &mut roomy(),
                &estimate(100, 100)
            )
            .is_err()
    );
}

// ---- TP-BUD-07: this month's spend comes back from the log ---------------------------------------

#[test]
fn tp_bud_07_seeding_from_the_log_keeps_only_the_latest_month() {
    let ledger = ledger(1_000, 1_000_000);
    // Replayed in log order: October's spend is dropped when November's appears.
    ledger.seed(&scout(), MID_NOVEMBER - 30 * 86_400_000, 700);
    ledger.seed(&scout(), MID_NOVEMBER, 300);
    ledger.seed(&grok(), MID_NOVEMBER, 50);
    assert_eq!(ledger.spent(&scout(), MID_NOVEMBER), 300);
    assert_eq!(ledger.spent(&grok(), MID_NOVEMBER), 50);
    assert_eq!(ledger.committed_all(MID_NOVEMBER), 350);
    assert_eq!(
        ledger.admit(&scout(), MID_NOVEMBER, &mut roomy(), &estimate(500, 16_384)),
        Err(BudgetError::OverMonth {
            cap: MonthlyCap::Agent,
            cap_micro_usd: 1_000,
            committed_micro_usd: 300,
            needed_micro_usd: 1_096,
        })
    );
}

// ---- Through the supervisor ----------------------------------------------------------------------

#[derive(Default)]
struct MemLog(Mutex<Vec<Event>>);

impl EventLog for MemLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        self.0.lock().expect("log").push(event.clone());
        Ok(())
    }
}

impl MemLog {
    fn failures(&self) -> Vec<(RunId, String)> {
        self.0
            .lock()
            .expect("log")
            .iter()
            .filter_map(|e| {
                let EventBody::RunFailed(f) = &e.body else {
                    return None;
                };
                Some((e.run_id.clone(), f.message.clone()))
            })
            .collect()
    }
}

/// Asks for one request (100 input tokens, up to 100 output: 800 µ$), reports `cost`, ends.
struct OneRequest {
    cost: u64,
}

impl RunDriver for OneRequest {
    fn run(&self, _request: RunRequest, sink: EventSink, _cancel: CancelToken) -> RunFuture {
        let cost = self.cost;
        Box::pin(async move {
            let request = ModelRequest {
                provider: "test".into(),
                model: "test-model".into(),
            };
            if sink.request_model(request, &estimate(100, 100)).is_err() {
                return;
            }
            sink.emit(EventBody::UsageReported(usage(100, 100, cost)))
                .expect("usage");
            sink.emit(EventBody::RunCompleted(RunCompleted { summary: None }))
                .expect("completed");
        })
    }
}

fn deploy(sup: &Supervisor, run: &str, agent: &str) {
    sup.deploy(RunRequest {
        run_id: RunId::from(run),
        agent_id: AgentId::from(agent),
        session_id: SessionId::from(run),
        prompt: "a task".into(),
    })
    .expect("deploy");
}

#[tokio::test(start_paused = true)]
async fn tp_bud_06_the_supervisor_holds_every_run_to_the_monthly_caps() {
    let log = Arc::new(MemLog::default());
    let ledger = Arc::new(ledger(1_000, 1_000_000));
    let sup = Supervisor::with_ledger(
        SupervisorConfig::default(),
        BudgetConfig::default(),
        Arc::clone(&ledger),
        Arc::new(OneRequest { cost: 800 }),
        log.clone(),
        Arc::new(ManualClock::at(MID_NOVEMBER)),
    );
    deploy(&sup, "r1", "scout");
    tokio::time::timeout(Duration::from_secs(60), sup.wait_idle())
        .await
        .expect("idle");
    deploy(&sup, "r2", "scout");
    deploy(&sup, "r3", "grok");
    tokio::time::timeout(Duration::from_secs(60), sup.wait_idle())
        .await
        .expect("idle");
    assert_eq!(
        log.failures(),
        [(
            RunId::from("r2"),
            "This agent would go over its $0.0010 monthly spend cap ($0.0008 used this month). Raise the cap or wait until next month.".to_owned()
        )],
        "Scout's second run is refused before anything is sent; Grok's isn't"
    );
    assert_eq!(ledger.spent(&scout(), MID_NOVEMBER), 800);
    assert_eq!(ledger.spent(&grok(), MID_NOVEMBER), 800);
    assert_eq!(ledger.committed_all(MID_NOVEMBER), 1_600);
    // It's the ledger the supervisor was given.
    assert_eq!(
        sup.ledger().config(),
        LedgerConfig {
            agent_month_cap_micro_usd: 1_000,
            global_month_cap_micro_usd: 1_000_000,
        }
    );
    assert_eq!(sup.ledger().committed_all(MID_NOVEMBER), 1_600);
}

#[test]
fn tp_bud_06_a_supervisor_gets_the_default_caps() {
    let sup = Supervisor::with_budget(
        SupervisorConfig::default(),
        BudgetConfig::default(),
        Arc::new(OneRequest { cost: 0 }),
        Arc::new(MemLog::default()),
        Arc::new(ManualClock::at(MID_NOVEMBER)),
    );
    assert_eq!(sup.ledger().config(), LedgerConfig::default());
}
