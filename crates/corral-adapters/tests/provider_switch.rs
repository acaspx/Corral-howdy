//! Turning a provider-side tool on for an agent (M3 item 4.2, TP-PROV-10, 13). The catalog says
//! which agents can be given which provider-side tools; the switch decides, when each run starts,
//! whether the run goes to the agent that has the tool or the one without it.

use corral_adapters::agent::ProviderTool;
use corral_adapters::catalog::{Switched, provider_tools};
use corral_core::clock::ManualClock;
use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId};
use corral_core::supervisor::{
    CancelToken, EventLog, EventSink, LogError, RunDriver, RunFuture, RunRequest, Supervisor,
    SupervisorConfig,
};
use corral_secrets::Provider;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[test]
fn tp_prov_10_only_claude_can_be_given_web_search_so_far() {
    assert_eq!(
        provider_tools(Provider::Anthropic),
        [ProviderTool::WebSearch]
    );
    for provider in [Provider::Xai, Provider::OpenAi, Provider::Meta] {
        assert!(provider_tools(provider).is_empty(), "{provider:?}");
    }
}

#[test]
fn tp_prov_10_a_provider_tool_is_saved_by_its_name() {
    assert_eq!(
        serde_json::to_value(ProviderTool::WebSearch).expect("json"),
        serde_json::json!("web_search")
    );
    assert_eq!(
        serde_json::from_value::<ProviderTool>(serde_json::json!("web_search")).expect("read"),
        ProviderTool::WebSearch
    );
    assert!(serde_json::from_value::<ProviderTool>(serde_json::json!("code_execution")).is_err());
}

#[derive(Default)]
struct MemLog(Mutex<Vec<Event>>);

impl EventLog for MemLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        self.0.lock().expect("lock").push(event.clone());
        Ok(())
    }
}

/// Completes with a summary naming itself, so the test sees which driver ran.
struct Named(&'static str);

impl RunDriver for Named {
    fn run(&self, _request: RunRequest, sink: EventSink, _cancel: CancelToken) -> RunFuture {
        let name = self.0;
        Box::pin(async move {
            sink.emit(EventBody::RunCompleted(RunCompleted {
                summary: Some(name.into()),
            }))
            .expect("completed");
        })
    }
}

async fn summary(sup: &Supervisor, log: &MemLog, run: &str) -> Option<String> {
    sup.deploy(RunRequest {
        run_id: RunId::from(run),
        agent_id: AgentId::from("claude"),
        session_id: SessionId::from("s1"),
        prompt: "hi".into(),
    })
    .expect("deploy");
    tokio::time::timeout(Duration::from_secs(10), sup.wait_idle())
        .await
        .expect("ends");
    let last = log.0.lock().expect("lock").last().cloned().expect("events");
    let EventBody::RunCompleted(done) = last.body else {
        panic!("expected run.completed");
    };
    done.summary
}

#[tokio::test(start_paused = true)]
async fn tp_prov_13_each_run_goes_where_the_switch_was_when_it_started() {
    let on = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&on);
    let switched = Switched::new(
        Arc::new(Named("searching")),
        Arc::new(Named("plain")),
        Arc::new(move || flag.load(Ordering::SeqCst)),
    );
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::new(
        SupervisorConfig::default(),
        Arc::new(switched),
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    assert_eq!(summary(&sup, &log, "r1").await.as_deref(), Some("plain"));
    on.store(true, Ordering::SeqCst);
    assert_eq!(
        summary(&sup, &log, "r2").await.as_deref(),
        Some("searching")
    );
    on.store(false, Ordering::SeqCst);
    assert_eq!(summary(&sup, &log, "r3").await.as_deref(), Some("plain"));
}
