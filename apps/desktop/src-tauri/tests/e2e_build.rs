//! The `e2e` build (Spike S2 step 4, TP-E2E-01..03): the bundled app the nightly drives with
//! XCUITest. Every provider is a scripted `FakeDriver` run by the real supervisor, keys stay in
//! memory and are accepted without asking anyone, and nothing reaches the network (AGENTS.md rule
//! 7). Only built with `--features e2e`; `scripts/ci/no_e2e_in_release.sh` keeps it out of a
//! release (TP-CI-17..19).
#![cfg(feature = "e2e")]

use corral_adapters::catalog::{AgentRouter, agent_for};
use corral_adapters::fake::FakeStep;
use corral_adapters::logos::LogoId;
use corral_adapters::transport::{Form, FormTransport};
use corral_core::clock::ManualClock;
use corral_core::event::{Event, EventBody};
use corral_core::ids::{RunId, SessionId};
use corral_core::supervisor::{EventLog, LogError, RunRequest, Supervisor, SupervisorConfig};
use corral_desktop_lib::e2e;
use corral_desktop_lib::keys::{connect_key, connected};
use corral_desktop_lib::logos::{LogoStore, fetch_missing};
use corral_secrets::Provider;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

/// A made-up key, as the nightly types it.
const KEY: &str = "xai-e2eNotARealKey4Hq8Vz2Nc6Tp1Lw"; // gitleaks:allow (made-up test key)

#[derive(Default)]
struct MemLog(Mutex<Vec<Event>>);

impl EventLog for MemLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        self.0.lock().expect("log").push(event.clone());
        Ok(())
    }
}

#[tokio::test]
async fn tp_e2e_01_keys_stay_in_memory_and_any_key_is_accepted_without_a_network() {
    let keys = e2e::keys();
    let plan = e2e::plan();
    assert!(
        connected(keys.0.as_ref(), &plan).is_empty(),
        "a first launch: no keys, ChatGPT signed out"
    );
    let checked = connect_key(
        keys.0.as_ref(),
        e2e::key_checker().0.as_ref(),
        Provider::Xai,
        KEY.to_owned(),
    )
    .await
    .expect("the e2e build's check accepts the key");
    assert!(checked.checked, "checked, by the e2e build's stand-in");
    assert!(checked.status.configured);
    assert_eq!(checked.status.last4.as_deref(), Some("p1Lw"));
    assert_eq!(connected(keys.0.as_ref(), &plan), [Provider::Xai]);
}

#[tokio::test(start_paused = true)]
async fn tp_e2e_02_every_agent_answers_the_scripted_answer_through_the_supervisor() {
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::new(
        SupervisorConfig::default(),
        Arc::new(AgentRouter::new(e2e::drivers())),
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    for provider in Provider::ALL {
        let agent = agent_for(provider).id;
        sup.deploy(RunRequest {
            run_id: RunId::from(format!("r-{}", provider.id()).as_str()),
            agent_id: agent,
            session_id: SessionId::from(format!("s-{}", provider.id()).as_str()),
            prompt: "What is 2 + 2?".into(),
        })
        .expect("deploy");
    }
    tokio::time::timeout(Duration::from_secs(3600), sup.wait_idle())
        .await
        .expect("idle");

    let events = log.0.lock().expect("log");
    for provider in Provider::ALL {
        let run = RunId::from(format!("r-{}", provider.id()).as_str());
        let of_run: Vec<&Event> = events.iter().filter(|e| e.run_id == run).collect();
        let said: String = of_run
            .iter()
            .filter_map(|e| {
                if let EventBody::ModelDelta(d) = &e.body {
                    Some(d.text.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(said, e2e::ANSWER, "{}'s answer", provider.id());
        assert!(
            of_run
                .iter()
                .any(|e| matches!(e.body, EventBody::RunCompleted(_))),
            "{} finished",
            provider.id()
        );
        let spent: u64 = of_run
            .iter()
            .filter_map(|e| {
                if let EventBody::UsageReported(u) = &e.body {
                    Some(u.cost_micro_usd)
                } else {
                    None
                }
            })
            .sum();
        assert!(
            spent > 0,
            "{} spent something, so the card has spend to show",
            provider.id()
        );
    }
    let waited: Duration = e2e::script()
        .steps()
        .iter()
        .filter_map(|s| {
            if let FakeStep::Wait(d) = s {
                Some(*d)
            } else {
                None
            }
        })
        .sum();
    assert!(
        waited >= Duration::from_secs(6),
        "long enough for the nightly to watch the ring and read the hover card tick"
    );
}

#[tokio::test]
async fn tp_e2e_03_nothing_reaches_the_network() {
    // ChatGPT: the plan's transport refuses every request.
    let offline = e2e::Offline;
    assert!(offline.get("https://auth.openai.com/").await.is_err());
    let form = Form::default();
    assert!(
        offline
            .post_form("https://auth.openai.com/oauth/token", &form)
            .await
            .is_err()
    );
    // Logos: a fixed picture instead of the provider's site, which the app's checks accept.
    let dir = std::env::temp_dir().join(format!("corral-e2e-logos-{}", std::process::id()));
    let store = LogoStore::new(dir.clone());
    let fetched = fetch_missing(&e2e::Logos, &store, &[LogoId::Provider(Provider::Xai)]).await;
    assert_eq!(fetched.len(), 1);
    assert!(
        fetched[0].1.is_ok(),
        "the stand-in logo is accepted: {:?}",
        fetched[0].1
    );
    // Cleanup only.
    let _ = std::fs::remove_dir_all(dir);
}

/// What `scripts/ci/no_e2e_in_release.sh` looks for in a release binary.
const RELEASE_CHECK_LOOKS_FOR: &str = "corral-e2e-build";

#[test]
fn tp_e2e_04_the_build_carries_the_mark_the_release_check_looks_for() {
    assert!(e2e::MARKER.starts_with(RELEASE_CHECK_LOOKS_FOR));
    assert!(
        e2e::MARKER.contains("fake providers"),
        "and says what it is when it starts"
    );
    let script = include_str!("../../../../scripts/ci/no_e2e_in_release.sh");
    assert!(
        script.contains(&format!("MARK='{RELEASE_CHECK_LOOKS_FOR}'")),
        "the release check looks for the same mark"
    );
}
