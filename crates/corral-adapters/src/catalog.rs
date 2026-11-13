//! Which agent each provider's key brings into the corral, and the driver that routes each run to
//! its agent's provider (TP-VIEW-06).
//!
//! One agent per connected provider for now: Grok for xAI, Claude for Anthropic, GPT for OpenAI
//! and Muse Spark for the Meta Model API. Custom agents (several per provider, personas, tools)
//! arrive with manifests in M5. A provider whose client hasn't landed yet still gets its drop, and
//! a task given to it ends with a plain message; nothing is sent anywhere.

use crate::linked::{LinkedApp, LinkedSettings};
use corral_core::corral_view::AgentCard;
use corral_core::event::{EventBody, FailureReason, RunFailed};
use corral_core::ids::AgentId;
use corral_core::supervisor::{CancelToken, EventSink, RunDriver, RunFuture, RunRequest};
use corral_secrets::Provider;
use std::collections::HashMap;
use std::sync::Arc;

/// The agent a provider's key brings.
pub fn agent_for(provider: Provider) -> AgentCard {
    let (id, name, monogram) = match provider {
        Provider::Xai => ("grok", "Grok", "G"),
        Provider::Anthropic => ("claude", "Claude", "C"),
        Provider::OpenAi => ("gpt", "GPT", "GP"),
        Provider::Meta => ("muse-spark", "Muse Spark", "MS"),
    };
    AgentCard {
        id: AgentId::from(id),
        name: name.into(),
        monogram: monogram.into(),
    }
}

/// The provider behind one of the catalog's agents.
pub fn provider_of(agent: &AgentId) -> Option<Provider> {
    Provider::ALL
        .into_iter()
        .find(|p| &agent_for(*p).id == agent)
}

/// The agents for the connected providers, in the order Settings lists providers (until usage
/// ranking lands, M4).
pub fn agents_for(connected: &[Provider]) -> Vec<AgentCard> {
    Provider::ALL
        .into_iter()
        .filter(|p| connected.contains(p))
        .map(agent_for)
        .collect()
}

/// Everything in the corral: the agents for the connected providers, then the linked apps the
/// user chose to show (ADR-005).
pub fn corral_agents(connected: &[Provider], linked: &LinkedSettings) -> Vec<AgentCard> {
    let mut cards = agents_for(connected);
    cards.extend(linked.shown().into_iter().map(LinkedApp::agent_card));
    cards
}

/// Runs each task with its agent's driver. An agent with no driver ends its run with a message.
pub struct AgentRouter {
    drivers: HashMap<AgentId, Arc<dyn RunDriver>>,
}

impl AgentRouter {
    /// A router over these drivers, by agent.
    pub fn new(drivers: HashMap<AgentId, Arc<dyn RunDriver>>) -> Self {
        Self { drivers }
    }
}

impl RunDriver for AgentRouter {
    fn run(&self, request: RunRequest, sink: EventSink, cancel: CancelToken) -> RunFuture {
        if let Some(driver) = self.drivers.get(&request.agent_id) {
            return driver.run(request, sink, cancel);
        }
        let name = provider_of(&request.agent_id)
            .map(|p| agent_for(p).name)
            .unwrap_or_else(|| request.agent_id.to_string());
        Box::pin(async move {
            // Refused only if the run already ended; the supervisor closes it out either way.
            let _ = sink.emit(EventBody::RunFailed(RunFailed {
                reason: FailureReason::Adapter,
                message: format!(
                    "{name} can't take tasks yet: its client arrives in an upcoming update."
                ),
            }));
        })
    }
}
