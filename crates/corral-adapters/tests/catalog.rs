//! Which agent each provider's key brings into the corral, and how a run reaches its provider
//! (TP-VIEW-06). A provider whose client hasn't landed yet still gets its drop; a task given to it
//! ends with a plain message instead of being sent anywhere.

use corral_adapters::catalog::{AgentRouter, agent_for, agents_for, provider_of};
use corral_core::clock::ManualClock;
use corral_core::corral_view::AgentCard;
use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId};
use corral_core::supervisor::{
    CancelToken, EventLog, EventSink, LogError, RunDriver, RunFuture, RunRequest, Supervisor,
    SupervisorConfig,
};
use corral_secrets::Provider;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn card(id: &str, name: &str, monogram: &str) -> AgentCard {
    AgentCard {
        id: AgentId::from(id),
        name: name.into(),
        monogram: monogram.into(),
    }
}

#[test]
fn tp_view_06_each_provider_brings_one_agent() {
    assert_eq!(agent_for(Provider::Xai), card("grok", "Grok", "G"));
    assert_eq!(agent_for(Provider::Anthropic), card("claude", "Claude", "C"));
    assert_eq!(agent_for(Provider::OpenAi), card("gpt", "GPT", "GP"));
    assert_eq!(agent_for(Provider::Meta), card("muse-spark", "Muse Spark", "MS"));
    for provider in Provider::ALL {
        assert_eq!(provider_of(&agent_for(provider).id), Some(provider));
    }
    assert_eq!(provider_of(&AgentId::from("stranger")), None);
}

#[test]
fn tp_view_06_agents_follow_the_connected_keys_in_a_stable_order() {
    let ids: Vec<String> = agents_for(&[Provider::Meta, Provider::Anthropic, Provider::Xai])
        .into_iter()
        .map(|c| c.id.to_string())
        .collect();
    assert_eq!(ids, ["grok", "claude", "muse-spark"]);
    assert!(agents_for(&[]).is_empty());
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

async fn run(agent: &str) -> Event {
    let mut drivers: HashMap<AgentId, Arc<dyn RunDriver>> = HashMap::new();
    drivers.insert(AgentId::from("grok"), Arc::new(Named("xai")));
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::new(
        SupervisorConfig::default(),
        Arc::new(AgentRouter::new(drivers)),
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    sup.deploy(RunRequest {
        run_id: RunId::from("r1"),
        agent_id: AgentId::from(agent),
        session_id: SessionId::from("s1"),
        prompt: "hi".into(),
    })
    .expect("deploy");
    tokio::time::timeout(Duration::from_secs(10), sup.wait_idle())
        .await
        .expect("ends");
    let last = log.0.lock().expect("lock").last().cloned();
    last.expect("events")
}

#[tokio::test(start_paused = true)]
async fn tp_view_06_a_run_reaches_its_agents_provider() {
    let last = run("grok").await;
    let EventBody::RunCompleted(done) = last.body else {
        panic!("expected run.completed");
    };
    assert_eq!(done.summary.as_deref(), Some("xai"));
}

#[tokio::test(start_paused = true)]
async fn tp_view_06_an_agent_without_a_client_yet_says_so() {
    let last = run("claude").await;
    let EventBody::RunFailed(failed) = last.body else {
        panic!("expected run.failed");
    };
    assert_eq!(failed.reason, FailureReason::Adapter);
    assert_eq!(
        failed.message,
        "Claude can't take tasks yet: its client arrives in an upcoming update."
    );
}
