//! What the nightly live check passes and fails on (M3 exit; EV-PROV-01, 03; docs/04 §8,
//! TP-SMOKE-01..04). `examples/live_smoke.rs` does the network part, in CI's `live-smoke`
//! environment; this decides, so it's tested offline.
//!
//! - **Keys** come from the environment's secrets, one variable per provider ([`KEY_VARS`]). This
//!   is the one place a key comes from anywhere but the Keychain: CI has no Keychain, and the
//!   environment holds a separate low-limit key per provider, used only from `main`. A blank
//!   variable is a provider without a key; no keys at all fails, so the check never passes having
//!   checked nothing. A key is only ever named by its variable.
//! - **EV-PROV-03:** the provider's model list, asked for the way the key check asks
//!   ([`models_request`]), still has the model the app's agent uses, and every model the agent
//!   editor offers ([`check_offered_models`]).
//! - **EV-PROV-01:** one short task ([`SMOKE_PROMPT`]) through the agent loop ends with one
//!   `run.completed` and nothing after it, an answer, and usage with tokens and a cost.

use corral_core::event::{Event, EventBody};
use corral_secrets::{Provider, Secret};
use serde_json::Value;

/// Where each provider's live key is, in the order they're checked.
pub const KEY_VARS: [(Provider, &str); 2] = [
    (Provider::Xai, "CORRAL_LIVE_XAI_KEY"),
    (Provider::Anthropic, "CORRAL_LIVE_ANTHROPIC_KEY"),
];

/// The task: short, benign, and with an answer that's easy to see.
pub const SMOKE_PROMPT: &str = "Reply with exactly these four words: Corral is ready now.";

/// The most one provider's task may spend: 5 cents.
pub const SMOKE_RUN_CAP_MICRO_USD: u64 = 50_000;

/// The keys to check, from `var` (the environment, in the example): each provider whose
/// variable holds something, in [`KEY_VARS`] order.
pub fn live_keys(var: impl Fn(&str) -> Option<String>) -> Result<Vec<(Provider, Secret)>, String> {
    let mut keys = Vec::new();
    for (provider, name) in KEY_VARS {
        let Some(value) = var(name).filter(|v| !v.trim().is_empty()) else {
            continue;
        };
        let secret =
            Secret::new(value).map_err(|e| format!("{name} doesn't hold a usable key: {e}"))?;
        keys.push((provider, secret));
    }
    if keys.is_empty() {
        let names: Vec<&str> = KEY_VARS.iter().map(|(_, name)| *name).collect();
        return Err(format!(
            "no live keys: set at least one of {} in the live-smoke environment",
            names.join(", ")
        ));
    }
    Ok(keys)
}

/// Where and how to ask `provider` for its models with `key`: the same request the key check
/// sends.
pub fn models_request(
    provider: Provider,
    key: &Secret,
) -> Option<(&'static str, Vec<(&'static str, String)>)> {
    crate::verify::models_request(provider, key)
}

/// Whether `provider`'s model list (`{"data": [{"id": …}, …]}`) has the model its agent uses.
pub fn check_models(provider: Provider, list: &Value) -> Result<(), String> {
    let name = provider.display_name();
    let Some(model) = crate::catalog::default_model(provider) else {
        return Err(format!("Corral has no model for {name} yet"));
    };
    let Some(data) = list.get("data").and_then(Value::as_array) else {
        return Err(format!("{name}'s model list couldn't be read"));
    };
    if data
        .iter()
        .any(|m| m.get("id").and_then(Value::as_str) == Some(model))
    {
        Ok(())
    } else {
        Err(format!(
            "{name} no longer lists {model}, the model Corral's agent uses"
        ))
    }
}

/// Whether `provider`'s model list has every model the agent editor offers for it
/// ([`crate::catalog::models`]); the error names the missing ones.
pub fn check_offered_models(provider: Provider, list: &Value) -> Result<(), String> {
    let name = provider.display_name();
    let offered = crate::catalog::models(provider);
    if offered.is_empty() {
        return Err(format!("Corral offers no models for {name} yet"));
    }
    let Some(data) = list.get("data").and_then(Value::as_array) else {
        return Err(format!("{name}'s model list couldn't be read"));
    };
    let listed = |id: &str| {
        data.iter()
            .any(|m| m.get("id").and_then(Value::as_str) == Some(id))
    };
    let missing: Vec<&str> = offered
        .iter()
        .map(|m| m.id)
        .filter(|id| !listed(id))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{name} no longer lists {}, offered in Corral's agent editor",
            missing.join(", ")
        ))
    }
}

/// What a passing run reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Input tokens, over all turns.
    pub input_tokens: u64,
    /// Output tokens, over all turns.
    pub output_tokens: u64,
    /// What it cost, in micro-USD.
    pub cost_micro_usd: u64,
    /// The last answer.
    pub answer: String,
}

/// Whether a run's events show the task done well, with its usage reported.
pub fn judge(events: &[Event]) -> Result<Outcome, String> {
    let ended = events.iter().position(|e| ends_the_run(&e.body));
    let Some(end) = ended else {
        return Err("the run didn't end".into());
    };
    if end + 1 != events.len() {
        return Err("events came after the run ended".into());
    }
    let (mut input, mut output, mut cost) = (0_u64, 0_u64, 0_u64);
    let mut reported = false;
    let mut answer = String::new();
    for event in events {
        if let EventBody::UsageReported(usage) = &event.body {
            reported = true;
            input = input.saturating_add(usage.input_tokens);
            output = output.saturating_add(usage.output_tokens);
            cost = cost.saturating_add(usage.cost_micro_usd);
        }
        if let EventBody::ModelResponse(response) = &event.body {
            answer.clone_from(&response.text);
        }
    }
    if let EventBody::RunFailed(failed) = &events[end].body {
        return Err(format!("the run failed: {}", failed.message));
    }
    if let EventBody::RunCancelled(_) = &events[end].body {
        return Err("the run was cancelled".into());
    }
    if !reported {
        return Err("no usage was reported".into());
    }
    if input == 0 {
        return Err("usage reported no input tokens".into());
    }
    if output == 0 {
        return Err("usage reported no output tokens".into());
    }
    if cost == 0 {
        return Err("usage reported no cost".into());
    }
    if answer.trim().is_empty() {
        return Err("the answer was empty".into());
    }
    Ok(Outcome {
        input_tokens: input,
        output_tokens: output,
        cost_micro_usd: cost,
        answer,
    })
}

/// Whether `body` ends a run.
fn ends_the_run(body: &EventBody) -> bool {
    match body {
        EventBody::RunCompleted(_) | EventBody::RunFailed(_) | EventBody::RunCancelled(_) => true,
        EventBody::RunQueued(_)
        | EventBody::RunStarted(_)
        | EventBody::PlanUpdated(_)
        | EventBody::ModelRequest(_)
        | EventBody::ModelDelta(_)
        | EventBody::ModelResponse(_)
        | EventBody::ToolRequested(_)
        | EventBody::PolicyDecided(_)
        | EventBody::CheckpointOpened(_)
        | EventBody::CheckpointResolved(_)
        | EventBody::ToolStarted(_)
        | EventBody::ToolResult(_)
        | EventBody::UsageReported(_)
        | EventBody::BudgetWarned(_)
        | EventBody::RunPaused(_)
        | EventBody::RunResumed(_)
        | EventBody::SessionClosed(_) => false,
    }
}
