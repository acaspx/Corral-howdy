//! GPT agents on a ChatGPT plan (ADR-017): TP-SUB-06 (the plan's models) and TP-SUB-07 (runs on
//! the plan), in `docs/04-test-plan.md`. OpenAI's "Models and inference" page for plan usage
//! (developers.openai.com/siwc/token-sharing-open-source/models-and-inference, checked
//! 2026-11-26): list with `GET /v1/models` and the plan's token, keep the entries whose
//! `visibility` is `list` in the server's order, show `display_name`, send `slug` as the model,
//! and fetch again when the account changes. A usage limit can arrive mid-stream as
//! `response.failed`. Nothing here touches the network (AGENTS.md rule 7).

use corral_adapters::cassette::{Cassette, CassetteTransport};
use corral_adapters::chatgpt::{self, ChatGptPlan, PlanAgent, PlanBuild, PlanModel};
use corral_adapters::responses::{ResponsesConfig, ResponsesDriver};
use corral_adapters::stream::{KeyFailure, KeySource};
use corral_adapters::transport::{BoxFuture, Form, FormTransport, TransportError};
use corral_core::clock::ManualClock;
use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId};
use corral_core::lifecycle::{Lifecycle, next};
use corral_core::supervisor::{
    EventLog, LogError, RunDriver, RunRequest, Supervisor, SupervisorConfig,
};
use corral_secrets::{MemoryPlanStore, PlanCredentials, PlanStore, Secret};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const NOW_MS: u64 = 1_790_001_000_000;
const ACCESS: &str = "plan-access-Zq81mXv3TtYb7cLp0wRn"; // gitleaks:allow (made-up test token)
const REFRESH: &str = "plan-refresh-Ux5bWk0aGm7rCz2fYq9"; // gitleaks:allow (made-up test token)
const ID: &str = "id-token-header.id-token-claims.id-token-signature"; // gitleaks:allow (made-up)

fn signed_in(subject: &str) -> PlanCredentials {
    PlanCredentials {
        email: format!("{subject}@example.test"),
        subject: subject.into(),
        client_id: "oaiapp_corral_test".into(),
        host_id: "urn:uuid:3f2b8c1e-9d4a-4e7b-8a6f-1c2d3e4f5a6b".into(),
        id_token: Secret::token(ID).expect("token"),
        access_token: Secret::token(ACCESS).expect("token"),
        refresh_token: Secret::token(REFRESH).expect("token"),
        expires_at_ms: NOW_MS + 3_600_000,
        scopes: vec!["chatgpt.tokens.use.direct".into()],
        saved_at_ms: NOW_MS,
    }
}

const CATALOG: &str = r#"{"models":[
  {"slug":"gpt-alpha","display_name":"GPT Alpha","visibility":"list"},
  {"slug":"gpt-hidden","display_name":"GPT Hidden","visibility":"hide"},
  {"slug":"gpt-beta","display_name":"GPT Beta","visibility":"list"},
  {"display_name":"No slug","visibility":"list"},
  {"slug":"gpt-gamma","visibility":"list"}
]}"#;

/// OpenAI's model list, answered from a script; keeps what was asked and with which token.
#[derive(Default)]
struct Catalog {
    replies: Mutex<Vec<(u16, String)>>,
    asked: Mutex<Vec<(String, String)>>,
}

impl Catalog {
    fn replying(replies: &[(u16, &str)]) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.iter().map(|(s, b)| (*s, (*b).to_owned())).collect()),
            asked: Mutex::default(),
        })
    }

    fn asked(&self) -> Vec<(String, String)> {
        self.asked.lock().expect("asked").clone()
    }
}

impl FormTransport for Catalog {
    fn post_form<'a>(
        &'a self,
        url: &'a str,
        _form: &'a Form,
    ) -> BoxFuture<'a, Result<(u16, Vec<u8>), TransportError>> {
        Box::pin(async move {
            Err(TransportError::Exhausted(format!(
                "no POST expected: {url}"
            )))
        })
    }

    fn get<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<(u16, Vec<u8>), TransportError>> {
        Box::pin(async move {
            Err(TransportError::Exhausted(format!(
                "no plain GET expected: {url}"
            )))
        })
    }

    fn get_authorized<'a>(
        &'a self,
        url: &'a str,
        bearer: &'a Secret,
    ) -> BoxFuture<'a, Result<(u16, Vec<u8>), TransportError>> {
        Box::pin(async move {
            self.asked
                .lock()
                .expect("asked")
                .push((url.to_owned(), bearer.expose().to_owned()));
            let mut replies = self.replies.lock().expect("replies");
            if replies.is_empty() {
                return Err(TransportError::Exhausted(format!(
                    "no reply scripted for {url}"
                )));
            }
            let (status, body) = replies.remove(0);
            Ok((status, body.into_bytes()))
        })
    }
}

fn plan(store: Arc<MemoryPlanStore>, catalog: Arc<Catalog>) -> Arc<ChatGptPlan> {
    Arc::new(ChatGptPlan::new(
        store,
        catalog,
        Arc::new(ManualClock::at(NOW_MS)),
    ))
}

fn signed_in_store(subject: &str) -> Arc<MemoryPlanStore> {
    let store = Arc::new(MemoryPlanStore::default());
    store.set_plan(&signed_in(subject)).expect("store");
    store
}

// ---- TP-SUB-06: the plan's models ----------------------------------------------------------

#[test]
fn tp_sub_06_the_list_keeps_listed_models_in_the_servers_order() {
    let models = chatgpt::listed_models(CATALOG.as_bytes()).expect("a catalog");
    let slugs: Vec<&str> = models.iter().map(|m| m.slug.as_str()).collect();
    assert_eq!(
        slugs,
        ["gpt-alpha", "gpt-beta", "gpt-gamma"],
        "hidden and slugless entries go"
    );
    assert_eq!(models[0].display_name, "GPT Alpha");
    assert_eq!(
        models[2].display_name, "gpt-gamma",
        "no display name: the slug is shown"
    );
    for bad in ["", "[]", r#"{"data":[]}"#, r#"{"models":"x"}"#] {
        assert!(chatgpt::listed_models(bad.as_bytes()).is_err(), "{bad:?}");
    }
    assert_eq!(
        chatgpt::listed_models(br#"{"models":[]}"#).expect("an empty catalog"),
        Vec::<PlanModel>::new()
    );
}

#[tokio::test]
async fn tp_sub_06_models_come_from_the_plan_with_its_token_once_per_account() {
    let store = signed_in_store("anton");
    let catalog = Catalog::replying(&[
        (200, CATALOG),
        (
            200,
            r#"{"models":[{"slug":"gpt-other","visibility":"list"}]}"#,
        ),
    ]);
    let plan = plan(store.clone(), catalog.clone());

    let models = plan.models().await.expect("models");
    assert_eq!(models.len(), 3);
    let asked = catalog.asked();
    assert_eq!(asked[0].0, "https://api.openai.com/v1/models");
    assert_eq!(asked[0].1, ACCESS, "the plan's access token");

    plan.models().await.expect("models again");
    assert_eq!(catalog.asked().len(), 1, "the same account's list is kept");

    // Another account signs in: its list is fetched.
    store.set_plan(&signed_in("someone-else")).expect("store");
    let models = plan.models().await.expect("the other account's models");
    assert_eq!(models[0].slug, "gpt-other");
    assert_eq!(catalog.asked().len(), 2);
}

#[tokio::test]
async fn tp_sub_06_without_a_sign_in_or_with_a_refusal_there_are_no_models() {
    let plan_out = plan(Arc::new(MemoryPlanStore::default()), Catalog::replying(&[]));
    match plan_out.models().await {
        Err(KeyFailure::Missing(message)) => assert!(message.contains("Continue with ChatGPT")),
        other => panic!("expected sign in, got {other:?}"),
    }
    let refused = plan(
        signed_in_store("anton"),
        Catalog::replying(&[(
            403,
            r#"{"error":{"code":"subscription_sharing_user_not_eligible"}}"#,
        )]),
    );
    match refused.models().await {
        Err(KeyFailure::Unavailable(message)) => {
            assert!(
                message.contains("isn't available on this ChatGPT plan"),
                "{message}"
            )
        }
        other => panic!("expected not eligible, got {other:?}"),
    }
}

// ---- TP-SUB-07: runs on the plan -------------------------------------------------------------

#[derive(Default)]
struct MemLog(Mutex<Vec<Event>>);

impl EventLog for MemLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        self.0.lock().expect("log lock").push(event.clone());
        Ok(())
    }
}

impl MemLog {
    fn events(&self) -> Vec<Event> {
        self.0.lock().expect("log lock").clone()
    }

    fn terminal(&self) -> EventBody {
        let mut state = Lifecycle::idle();
        for e in self.events() {
            state = next(&state, &e).unwrap_or_else(|err| panic!("illegal event: {err}"));
        }
        self.events().last().cloned().expect("events").body
    }
}

/// Builds the real plan driver over a cassette, and keeps which model each run was built for.
struct Built {
    models: Mutex<Vec<String>>,
    transport: Arc<CassetteTransport>,
}

fn builder(cassette: &str, keys: Arc<dyn KeySource>) -> (Arc<Built>, PlanBuild) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/cassettes/openai-plan")
        .join(cassette);
    let built = Arc::new(Built {
        models: Mutex::default(),
        transport: Arc::new(CassetteTransport::new(
            Cassette::load(&path).expect("cassette"),
            4096,
        )),
    });
    let seen = built.clone();
    let build: PlanBuild = Arc::new(move |config: ResponsesConfig| {
        seen.models
            .lock()
            .expect("models")
            .push(config.model.clone());
        Arc::new(ResponsesDriver::with_keys(
            seen.transport.clone(),
            keys.clone(),
            config,
        )) as Arc<dyn RunDriver>
    });
    (built, build)
}

async fn run(driver: Arc<dyn RunDriver>) -> Arc<MemLog> {
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::new(
        SupervisorConfig::default(),
        driver,
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    sup.deploy(RunRequest {
        run_id: RunId::from("r1"),
        agent_id: AgentId::from("gpt"),
        session_id: SessionId::from("s1"),
        prompt: "Say hello.".into(),
    })
    .expect("deploy");
    tokio::time::timeout(Duration::from_secs(600), sup.wait_idle())
        .await
        .expect("the run ends");
    log
}

#[tokio::test(start_paused = true)]
async fn tp_sub_07_gpt_runs_on_the_plans_first_model_at_no_cost() {
    let plan = plan(
        signed_in_store("anton"),
        Catalog::replying(&[(200, CATALOG)]),
    );
    let (built, build) = builder("text-ok.json", plan.clone());
    let log = run(Arc::new(PlanAgent::new(plan, None, build, true))).await;
    assert!(
        matches!(log.terminal(), EventBody::RunCompleted(_)),
        "{:?}",
        log.terminal()
    );
    assert_eq!(
        *built.models.lock().expect("models"),
        ["gpt-alpha"],
        "the server's first"
    );
    let sent = &built.transport.seen()[0];
    assert_eq!(sent.body["model"], "gpt-alpha");
    assert_eq!(sent.bearer_last4, &ACCESS[ACCESS.len() - 4..]);
    assert!(
        sent.body.get("max_output_tokens").is_none(),
        "the plan's route"
    );
    let spent: u64 = log
        .events()
        .iter()
        .filter_map(|e| {
            if let EventBody::UsageReported(u) = &e.body {
                Some(u.cost_micro_usd)
            } else {
                None
            }
        })
        .sum();
    assert_eq!(spent, 0);
}

#[tokio::test(start_paused = true)]
async fn tp_sub_07_a_chosen_model_is_used_while_the_plan_lists_it() {
    let plan = plan(
        signed_in_store("anton"),
        Catalog::replying(&[(200, CATALOG)]),
    );
    let (built, build) = builder("text-ok.json", plan.clone());
    let agent = PlanAgent::new(plan.clone(), Some("gpt-beta".into()), build, true);
    run(Arc::new(agent)).await;
    assert_eq!(*built.models.lock().expect("models"), ["gpt-beta"]);

    let (built, build) = builder("text-ok.json", plan.clone());
    let agent = PlanAgent::new(plan, Some("gpt-retired".into()), build, true);
    run(Arc::new(agent)).await;
    assert_eq!(
        *built.models.lock().expect("models"),
        ["gpt-alpha"],
        "a model the plan no longer lists falls back to its first"
    );
}

#[tokio::test(start_paused = true)]
async fn tp_sub_07_no_sign_in_or_no_models_fails_the_run_before_any_request() {
    let plan_out = plan(Arc::new(MemoryPlanStore::default()), Catalog::replying(&[]));
    let (built, build) = builder("text-ok.json", plan_out.clone());
    let log = run(Arc::new(PlanAgent::new(plan_out, None, build, true))).await;
    let EventBody::RunFailed(f) = log.terminal() else {
        panic!("expected run.failed");
    };
    assert_eq!(f.reason, FailureReason::Provider);
    assert!(f.message.contains("Continue with ChatGPT"), "{}", f.message);
    assert!(
        built.models.lock().expect("models").is_empty(),
        "nothing was built"
    );

    let empty = plan(
        signed_in_store("anton"),
        Catalog::replying(&[(200, r#"{"models":[]}"#)]),
    );
    let (built, build) = builder("text-ok.json", empty.clone());
    let log = run(Arc::new(PlanAgent::new(empty, None, build, true))).await;
    let EventBody::RunFailed(f) = log.terminal() else {
        panic!("expected run.failed");
    };
    assert!(f.message.contains("offers no models"), "{}", f.message);
    assert!(built.transport.seen().is_empty());
}

#[tokio::test(start_paused = true)]
async fn tp_sub_07_a_usage_limit_mid_stream_reads_like_one_up_front() {
    let plan = plan(
        signed_in_store("anton"),
        Catalog::replying(&[(200, CATALOG)]),
    );
    let (_, build) = builder("usage-limit-mid-stream.json", plan.clone());
    let log = run(Arc::new(PlanAgent::new(plan, None, build, true))).await;
    let EventBody::RunFailed(f) = log.terminal() else {
        panic!("expected run.failed");
    };
    assert!(f.message.contains("Usage limit reached"), "{}", f.message);
    assert!(
        f.message.contains(chatgpt::MANAGE_USAGE_URL),
        "{}",
        f.message
    );
}

#[test]
fn tp_sub_07_the_agent_says_whether_it_pauses() {
    let plan = plan(Arc::new(MemoryPlanStore::default()), Catalog::replying(&[]));
    let (_, build) = builder("text-ok.json", Arc::new(NoKey));
    let gpt = AgentId::from("gpt");
    assert!(PlanAgent::new(plan.clone(), None, build.clone(), true).pauses(&gpt));
    assert!(!PlanAgent::new(plan, None, build, false).pauses(&gpt));
}

struct NoKey;

impl KeySource for NoKey {
    fn key(
        &self,
        _provider: corral_secrets::Provider,
    ) -> BoxFuture<'_, Result<Secret, KeyFailure>> {
        Box::pin(async { Err(KeyFailure::Missing("no key".into())) })
    }
}
