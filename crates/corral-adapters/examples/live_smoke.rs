//! The nightly live check (M3 exit; EV-PROV-01, 03). Runs in CI's `live-smoke` environment:
//!
//!     cargo run -p corral-adapters --example live_smoke
//!
//! For each provider with a key in its variable (`corral_adapters::smoke::KEY_VARS`), it asks the
//! provider whether it still lists the model Corral's agent uses, then sends one short task
//! through the same agent loop the app uses, with no tools and a 5-cent cap, and checks that the
//! run completes with its usage reported. What passes or fails is `corral_adapters::smoke`. It
//! prints only what each provider said and what the task cost, never a key or a header, and exits
//! non-zero if anything failed.

use corral_adapters::agent::{MessagesAgent, ResponsesAgent, ToolSetup};
use corral_adapters::catalog::{agent_for, default_model};
use corral_adapters::messages::MessagesConfig;
use corral_adapters::responses::ResponsesConfig;
use corral_adapters::smoke::{
    SMOKE_PROMPT, SMOKE_RUN_CAP_MICRO_USD, check_models, check_offered_models, judge, live_keys,
    models_request,
};
use corral_adapters::transport::{ReqwestTransport, Transport};
use corral_core::budget::BudgetConfig;
use corral_core::clock::SystemClock;
use corral_core::event::Event;
use corral_core::ids::{RunId, SessionId};
use corral_core::supervisor::{
    EventLog, LogError, RunDriver, RunRequest, Supervisor, SupervisorConfig,
};
use corral_policy::{Policy, Profile};
use corral_secrets::{MemoryStore, Provider, Secret, SecretStore};
use corral_tools::{CheckpointDesk, ToolGateway};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The longest one provider's task may take.
const RUN_TIMEOUT: Duration = Duration::from_secs(180);

fn main() {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("live_smoke: couldn't start: {e}");
            std::process::exit(1);
        }
    };
    let failures = runtime.block_on(run());
    if failures > 0 {
        eprintln!("live_smoke: {failures} check(s) failed");
        std::process::exit(1);
    }
    println!("live_smoke: all checks passed");
}

/// Runs every check and returns how many failed.
async fn run() -> usize {
    let keys = match live_keys(|name| std::env::var(name).ok()) {
        Ok(keys) => keys,
        Err(message) => {
            eprintln!("live_smoke: {message}");
            return 1;
        }
    };
    let mut failures = 0;
    for (provider, key) in keys {
        let name = provider.display_name();
        match models(provider, &key).await {
            Ok(()) => println!("{name}: EV-PROV-03 ok, the model is listed"),
            Err(message) => {
                failures += 1;
                eprintln!("{name}: EV-PROV-03 failed: {message}");
            }
        }
        match task(provider, key).await {
            Ok(summary) => println!("{name}: EV-PROV-01 ok, {summary}"),
            Err(message) => {
                failures += 1;
                eprintln!("{name}: EV-PROV-01 failed: {message}");
            }
        }
    }
    failures
}

/// EV-PROV-03: the provider still lists the model Corral's agent uses.
async fn models(provider: Provider, key: &Secret) -> Result<(), String> {
    let Some((url, headers)) = models_request(provider, key) else {
        return Err("Corral can't list this provider's models yet".into());
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.without_url().to_string())?;
    // Anthropic lists 20 models a page by default; ask for all of them in one.
    let url = if provider == Provider::Anthropic {
        format!("{url}?limit=1000")
    } else {
        url.to_owned()
    };
    let mut request = client.get(url);
    for (header, value) in headers {
        let mut value = reqwest::header::HeaderValue::from_str(&value)
            .map_err(|_| "the key can't be sent as a header".to_owned())?;
        // Never printed in debug output.
        value.set_sensitive(true);
        request = request.header(header, value);
    }
    let response = request
        .send()
        .await
        .map_err(|e| e.without_url().to_string())?;
    let status = response.status();
    if !status.is_success() {
        // The body isn't printed: some providers echo part of a refused key.
        return Err(format!("the model list answered {}", status.as_u16()));
    }
    let list: Value = response
        .json()
        .await
        .map_err(|e| e.without_url().to_string())?;
    check_models(provider, &list)?;
    check_offered_models(provider, &list)
}

#[derive(Default)]
struct MemLog(Mutex<Vec<Event>>);

impl EventLog for MemLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        self.0
            .lock()
            .map_err(|_| LogError("the log is poisoned".into()))?
            .push(event.clone());
        Ok(())
    }
}

/// EV-PROV-01: one short task through the agent loop completes with its usage reported.
async fn task(provider: Provider, key: Secret) -> Result<String, String> {
    let Some(model) = default_model(provider) else {
        return Err("Corral has no agent for this provider yet".into());
    };
    let keys = MemoryStore::default();
    keys.set(provider, &key).map_err(|e| e.to_string())?;
    let keys: Arc<dyn SecretStore> = Arc::new(keys);
    let transport: Arc<dyn Transport> =
        Arc::new(ReqwestTransport::new().map_err(|e| e.to_string())?);
    let n = Arc::new(AtomicU64::new(0));
    // No tools: the task only answers.
    let setup = ToolSetup {
        policy: Policy::new(Profile::default()),
        gateway: Arc::new(ToolGateway::new()),
        desk: Arc::new(CheckpointDesk::default()),
        ids: Arc::new(move || format!("smoke-{}", n.fetch_add(1, Ordering::Relaxed))),
    };
    let agent: Arc<dyn RunDriver> = match provider {
        Provider::Xai => Arc::new(ResponsesAgent::new(
            transport,
            keys,
            ResponsesConfig::xai(model),
            setup,
        )),
        Provider::Anthropic => Arc::new(MessagesAgent::new(
            transport,
            keys,
            MessagesConfig::anthropic(model),
            setup,
        )),
        Provider::OpenAi | Provider::Meta => {
            return Err("Corral has no agent for this provider yet".into());
        }
    };
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::with_budget(
        SupervisorConfig::default(),
        BudgetConfig {
            per_run_cap_micro_usd: SMOKE_RUN_CAP_MICRO_USD,
        },
        agent,
        log.clone(),
        Arc::new(SystemClock),
    );
    sup.deploy(RunRequest {
        run_id: RunId::from("smoke"),
        agent_id: agent_for(provider).id,
        session_id: SessionId::from("smoke"),
        prompt: SMOKE_PROMPT.into(),
    })
    .map_err(|e| e.to_string())?;
    tokio::time::timeout(RUN_TIMEOUT, sup.wait_idle())
        .await
        .map_err(|_| format!("the task took over {} s", RUN_TIMEOUT.as_secs()))?;
    let events = log
        .0
        .lock()
        .map_err(|_| "the log is poisoned".to_owned())?
        .clone();
    let outcome = judge(&events)?;
    Ok(format!(
        "{} in / {} out tokens, ${:.4}, answered {:?}",
        outcome.input_tokens,
        outcome.output_tokens,
        outcome.cost_micro_usd as f64 / 1_000_000.0,
        outcome.answer.chars().take(80).collect::<String>()
    ))
}
