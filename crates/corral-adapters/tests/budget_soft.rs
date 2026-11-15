//! The soft limit end to end (M2 item 5, TP-BUD-02): a FakeAdapter run that reaches its soft
//! limit stops at a `budget.threshold` checkpoint on its card. Approve lets it go on up to the
//! hard cap; Deny, Stop or no answer ends it, and nothing more is requested. The policy decides
//! whether to ask at all (a profile can allow or deny going past the soft limit).

use corral_adapters::fake::{FakeDriver, FakeScript, ToolSetup};
use corral_core::budget::Price;
use corral_core::clock::ManualClock;
use corral_core::domain::{ActionClass, Risk};
use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId};
use corral_core::lifecycle::{Lifecycle, next};
use corral_core::supervisor::{EventLog, LogError, RunRequest, Supervisor, SupervisorConfig};
use corral_policy::{Approval, Policy, Profile, Scope};
use corral_tools::{Answer, CheckpointDesk, ToolGateway};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PRICE: Price = Price::per_million_tokens(2_000_000, 6_000_000);
/// $0.50 of the default $1 cap.
const SOFT: u64 = 500_000;

#[derive(Default)]
struct MemLog(Mutex<Vec<Event>>);

impl EventLog for MemLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        self.0.lock().expect("log").push(event.clone());
        Ok(())
    }
}

impl MemLog {
    fn events(&self) -> Vec<Event> {
        self.0.lock().expect("log").clone()
    }

    fn kinds(&self) -> Vec<&'static str> {
        self.events().iter().map(|e| e.kind().as_str()).collect()
    }

    fn count(&self, kind: &str) -> usize {
        self.kinds().iter().filter(|k| **k == kind).count()
    }

    fn opened(&self) -> Vec<CheckpointOpened> {
        self.events()
            .into_iter()
            .filter_map(|e| {
                let EventBody::CheckpointOpened(c) = e.body else {
                    return None;
                };
                Some(c)
            })
            .collect()
    }

    fn terminal(&self) -> EventBody {
        self.events().last().expect("events").body.clone()
    }

    fn assert_legal(&self) {
        let mut state = Lifecycle::idle();
        for e in self.events() {
            state = next(&state, &e).unwrap_or_else(|err| panic!("illegal event: {err}"));
        }
    }
}

struct World {
    sup: Supervisor,
    log: Arc<MemLog>,
    desk: Arc<CheckpointDesk>,
}

/// Spends $0.60 in its first turn, then asks the model again.
fn spender() -> FakeScript {
    FakeScript::default()
        .say("Working on it.")
        .usage(1_000, 1_000, 600_000)
        .request()
        .say(" Done.")
}

fn world(profile: &str, tools: bool) -> World {
    let desk = Arc::new(CheckpointDesk::default());
    let mut driver = FakeDriver::new(spender()).priced(PRICE);
    if tools {
        let n = Arc::new(AtomicU64::new(0));
        driver = driver.with_tools(ToolSetup {
            policy: Policy::new(Profile::from_toml(profile).expect("profile")),
            gateway: Arc::new(ToolGateway::new()),
            desk: Arc::clone(&desk),
            ids: Arc::new(move || format!("id-{}", n.fetch_add(1, Ordering::SeqCst) + 1)),
        });
    }
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::new(
        SupervisorConfig::default(),
        Arc::new(driver),
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    sup.set_soft_limit(Some(SOFT));
    sup.deploy(RunRequest {
        run_id: RunId::from("r1"),
        agent_id: AgentId::from("fake"),
        session_id: SessionId::from("s1"),
        prompt: "Plan my week.".into(),
    })
    .expect("deploy");
    World { sup, log, desk }
}

async fn settle() {
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
}

async fn idle(w: &World) {
    tokio::time::timeout(Duration::from_secs(7 * 24 * 3600), w.sup.wait_idle())
        .await
        .expect("the run ends");
    w.log.assert_legal();
}

async fn waiting(w: &World) -> CheckpointOpened {
    settle().await;
    w.log
        .opened()
        .pop()
        .unwrap_or_else(|| panic!("no checkpoint: {:?}", w.log.kinds()))
}

#[tokio::test(start_paused = true)]
async fn tp_bud_02_the_soft_limit_asks_on_the_card_and_approve_goes_on() {
    let w = world("", true);
    let cp = waiting(&w).await;
    assert_eq!(cp.action_class, ActionClass::BudgetThreshold);
    assert_eq!(cp.risk, Risk::Normal);
    assert_eq!(cp.title, "Going past the spend limit");
    assert_eq!(
        cp.detail,
        "This run has used $0.60; its soft limit is $0.50.\nApprove to let it go on, up to its $1.00 cap."
    );
    assert_eq!(
        w.log.count("model.request"),
        1,
        "nothing more requested while it waits"
    );
    assert_eq!(w.log.count("budget.warned"), 1);
    w.desk
        .answer(
            &cp.checkpoint_id,
            Answer::Approve {
                how: Approval::Click,
                scope: Scope::Once,
            },
        )
        .expect("approved");
    idle(&w).await;
    assert_eq!(w.log.count("model.request"), 2);
    assert_eq!(w.log.count("budget.warned"), 1, "once per threshold");
    assert!(matches!(w.log.terminal(), EventBody::RunCompleted(_)));
}

#[tokio::test(start_paused = true)]
async fn tp_bud_02_deny_ends_the_run_and_requests_nothing_more() {
    let w = world("", true);
    let cp = waiting(&w).await;
    w.desk
        .answer(&cp.checkpoint_id, Answer::Deny { note: None })
        .expect("denied");
    idle(&w).await;
    assert_eq!(w.log.count("model.request"), 1);
    assert_eq!(
        w.log.terminal(),
        EventBody::RunCancelled(RunCancelled {
            by: CancelledBy::User
        })
    );
}

#[tokio::test(start_paused = true)]
async fn tp_bud_02_no_answer_ends_the_run_at_the_timeout_whatever_on_timeout_says() {
    let w = world(r#"checkpoint_timeout = "30m""#, true);
    let start = tokio::time::Instant::now();
    idle(&w).await;
    assert_eq!(start.elapsed(), Duration::from_secs(30 * 60));
    assert_eq!(w.log.count("model.request"), 1);
    assert_eq!(
        w.log.terminal(),
        EventBody::RunCancelled(RunCancelled {
            by: CancelledBy::CheckpointExpired
        }),
        "an unanswered soft limit never means spend more"
    );
    assert!(w.desk.is_empty());
}

#[tokio::test(start_paused = true)]
async fn tp_bud_02_stop_while_it_waits_ends_the_run() {
    let w = world("", true);
    let cp = waiting(&w).await;
    w.sup.cancel(&RunId::from("r1")).await.expect("stop");
    idle(&w).await;
    assert_eq!(w.log.count("model.request"), 1);
    assert!(matches!(w.log.terminal(), EventBody::RunCancelled(_)));
    assert!(w.desk.is_empty(), "the checkpoint left the desk");
    assert!(
        w.desk
            .answer(&cp.checkpoint_id, Answer::Deny { note: None })
            .is_err()
    );
}

#[tokio::test(start_paused = true)]
async fn tp_bud_02_a_profile_can_allow_or_forbid_going_past_the_soft_limit() {
    let allows = world(
        r#"rules = [{ class = "budget.threshold", decision = "allow" }]"#,
        true,
    );
    idle(&allows).await;
    assert!(allows.log.opened().is_empty(), "no checkpoint");
    assert_eq!(allows.log.count("model.request"), 2);
    assert_eq!(allows.log.count("budget.warned"), 1, "still recorded");
    assert!(matches!(allows.log.terminal(), EventBody::RunCompleted(_)));

    let forbids = world(
        r#"rules = [{ class = "budget.threshold", decision = "deny" }]"#,
        true,
    );
    idle(&forbids).await;
    assert_eq!(forbids.log.count("model.request"), 1);
    let EventBody::RunFailed(failed) = forbids.log.terminal() else {
        panic!("run.failed, got {:?}", forbids.log.kinds());
    };
    assert_eq!(failed.reason, FailureReason::BudgetHardCap);
    assert!(failed.message.contains("soft limit"), "{}", failed.message);
}

#[tokio::test(start_paused = true)]
async fn tp_bud_02_an_agent_that_cant_ask_stops_at_the_soft_limit() {
    let w = world("", false);
    idle(&w).await;
    assert_eq!(w.log.count("model.request"), 1);
    let EventBody::RunFailed(failed) = w.log.terminal() else {
        panic!("run.failed, got {:?}", w.log.kinds());
    };
    assert_eq!(failed.reason, FailureReason::BudgetHardCap);
    assert!(
        failed
            .message
            .starts_with("This run reached its $0.50 soft limit"),
        "{}",
        failed.message
    );
}

/// Grok and Claude can't ask the user yet (their tools come with M3), so at a soft limit they
/// stop plainly, sending nothing.
#[tokio::test(start_paused = true)]
async fn tp_bud_02_a_provider_agent_stops_at_the_soft_limit_sending_nothing() {
    use corral_adapters::cassette::{Cassette, CassetteTransport};
    use corral_adapters::responses::{ResponsesConfig, ResponsesDriver};
    use corral_secrets::{MemoryStore, Provider, Secret, SecretStore};

    let keys = MemoryStore::default();
    keys.set(
        Provider::Xai,
        &Secret::new("xai-Q7v2LmN9pRt4sWx8yZa3bCd5eFg6hJk1").expect("key"), // gitleaks:allow (made-up test key)
    )
    .expect("store");
    let cassette = Cassette::load(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cassettes/xai/text-ok.json"),
    )
    .expect("cassette");
    let transport = Arc::new(CassetteTransport::new(cassette, 4096));
    let driver = ResponsesDriver::new(
        transport.clone(),
        Arc::new(keys),
        ResponsesConfig::xai("grok-test"),
    );
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::new(
        SupervisorConfig::default(),
        Arc::new(driver),
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    sup.set_soft_limit(Some(1));
    sup.deploy(RunRequest {
        run_id: RunId::from("r1"),
        agent_id: AgentId::from("grok"),
        session_id: SessionId::from("s1"),
        prompt: "Say hello.".into(),
    })
    .expect("deploy");
    tokio::time::timeout(Duration::from_secs(600), sup.wait_idle())
        .await
        .expect("the run ends");
    assert!(transport.seen().is_empty(), "nothing sent");
    let EventBody::RunFailed(failed) = log.terminal() else {
        panic!("run.failed, got {:?}", log.kinds());
    };
    assert_eq!(failed.reason, FailureReason::BudgetHardCap);
    assert!(failed.message.contains("soft limit"), "{}", failed.message);
}
