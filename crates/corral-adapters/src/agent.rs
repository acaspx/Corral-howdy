//! Grok with tools: the agent loop (M3 item 3, docs/01 §4–5, TP-TOOL-07..12).
//!
//! A run is a series of turns. Each turn is one Responses request carrying the whole
//! [`Conversation`] and the agent's offered tools, admitted by the budget meter like any request.
//! When the model calls tools, each call goes through the agent's [`ToolRunner`]: the policy
//! decides, an Ask waits at a checkpoint on the agent's card, and only an allowed call runs, in
//! the gateway. What came back (the output, or why it didn't run) goes into the next turn. The run
//! completes when the model answers without calling anything.
//!
//! - **Names.** A call names a tool by its wire name; [`ToolGateway::from_wire`] maps it back to
//!   the registered tool. A name the model wasn't offered stays as it is, matches no tool, and is
//!   `unknown`, which the policy denies without asking.
//! - **Arguments** must be a JSON object. Anything else runs nothing, logs nothing, and the model
//!   is told.
//! - **Stop** drops a request in flight at once; a call waiting at a checkpoint is taken off the
//!   desk by the runner, so a later answer does nothing. No request is sent after Stop.
//! - **The soft limit** (TP-BUD-02): when the next turn would go past it, the runner asks on the
//!   card. Approve lifts the limit to the hard cap and the turn goes ahead; Deny, Stop or no
//!   answer ends the run.
//! - **A cap on turns** ([`DEFAULT_MAX_TURNS`]): a run that keeps calling tools without finishing
//!   fails with a plain message instead of spending on.

use crate::responses::{Conversation, ResponseDecoder, ResponsesConfig};
use crate::stream::{Failure, StreamDriver, fail_run};
use crate::transport::Transport;
use corral_core::event::FailureReason;
use corral_core::supervisor::{CancelToken, EventSink, RunDriver, RunFuture, RunRequest};
use corral_policy::Policy;
use corral_secrets::SecretStore;
use corral_tools::{
    BudgetAnswer, CheckpointDesk, IdSource, OfferedTool, ToolAnswer, ToolGateway, ToolRunner,
};
use serde_json::Value;
use std::sync::Arc;

/// The most model turns a run gets before Corral stops it.
pub const DEFAULT_MAX_TURNS: u32 = 20;

/// What the model is told when a call's arguments aren't a JSON object.
const BAD_ARGUMENTS: &str =
    "Corral couldn't read this call: its arguments must be a JSON object. Nothing ran.";

/// What an agent's tool calls go through: its policy, its tools and the desk where checkpoints
/// wait. The fake agent uses the same.
#[derive(Clone)]
pub struct ToolSetup {
    /// The agent's policy. Each run gets a fresh gate under it.
    pub policy: Policy,
    /// The registered tools.
    pub gateway: Arc<ToolGateway>,
    /// Where checkpoints wait.
    pub desk: Arc<CheckpointDesk>,
    /// Tool-call and checkpoint IDs.
    pub ids: IdSource,
}

/// A [`RunDriver`] for an agent with tools over a Responses API.
pub struct ResponsesAgent {
    stream: StreamDriver<ResponsesConfig>,
    tools: ToolSetup,
    max_turns: u32,
}

impl ResponsesAgent {
    /// An agent that sends through `transport`, reads keys from `keys`, and runs its tool calls
    /// through `tools`.
    pub fn new(
        transport: Arc<dyn Transport>,
        keys: Arc<dyn SecretStore>,
        config: ResponsesConfig,
        tools: ToolSetup,
    ) -> Self {
        Self {
            stream: StreamDriver::new(transport, keys, config),
            tools,
            max_turns: DEFAULT_MAX_TURNS,
        }
    }

    /// At most `turns` model turns per run (at least one).
    pub fn with_max_turns(mut self, turns: u32) -> Self {
        self.max_turns = turns.max(1);
        self
    }
}

impl RunDriver for ResponsesAgent {
    fn run(&self, request: RunRequest, sink: EventSink, cancel: CancelToken) -> RunFuture {
        let setup = self.tools.clone();
        let agent = Agent {
            stream: self.stream.clone(),
            runner: ToolRunner::new(
                setup.policy,
                Arc::clone(&setup.gateway),
                setup.desk,
                setup.ids,
            ),
            offered: setup.gateway.offered(),
            gateway: setup.gateway,
            max_turns: self.max_turns,
        };
        // The loop watches Stop itself: dropping it at a checkpoint would leave the checkpoint
        // on the desk.
        Box::pin(async move { agent.work(&request.prompt, &sink, &cancel).await })
    }
}

/// One run's agent: a fresh runner, so no grants or taint carry over (TP-POL-04).
struct Agent {
    stream: StreamDriver<ResponsesConfig>,
    runner: ToolRunner,
    gateway: Arc<ToolGateway>,
    offered: Vec<OfferedTool>,
    max_turns: u32,
}

/// How a turn's request ended.
enum Sent {
    /// The response is in the decoder.
    Answered,
    /// The run is over: stopped, or failed and logged.
    Over,
}

impl Agent {
    async fn work(&self, prompt: &str, sink: &EventSink, cancel: &CancelToken) {
        let mut conversation = Conversation::new(prompt);
        for _ in 0..self.max_turns {
            let mut decoder = ResponseDecoder::offering_tools();
            if let Sent::Over = self.send(&conversation, &mut decoder, sink, cancel).await {
                return;
            }
            let calls = decoder.take_calls();
            // No calls: the decoder logged run.completed (or run.failed).
            if calls.is_empty() {
                return;
            }
            conversation.said(decoder.said());
            for call in calls {
                let Ok(Value::Object(args)) = serde_json::from_str::<Value>(&call.arguments) else {
                    conversation.called(&call, BAD_ARGUMENTS);
                    continue;
                };
                let tool = self.gateway.from_wire(&call.name).unwrap_or(&call.name);
                let output = match self
                    .runner
                    .call(tool, Value::Object(args), sink, cancel)
                    .await
                {
                    ToolAnswer::Ran { output, .. } => output,
                    ToolAnswer::Failed { message } => format!("That didn't work: {message}"),
                    ToolAnswer::Denied {
                        reason,
                        note: Some(note),
                    } => format!("{reason} The user's note: {note}"),
                    ToolAnswer::Denied { reason, note: None } => reason,
                    ToolAnswer::Stopped => return,
                };
                conversation.called(&call, &output);
            }
        }
        fail_run(
            sink,
            FailureReason::Adapter,
            format!(
                "This run used all {} turns without finishing, so Corral stopped it. Try a \
                 smaller task.",
                self.max_turns
            ),
        );
    }

    /// Sends one turn. At the soft limit it asks on the card, and sends if approved: approving
    /// lifts the limit to the hard cap, so the turn is asked about once.
    async fn send(
        &self,
        conversation: &Conversation,
        decoder: &mut ResponseDecoder,
        sink: &EventSink,
        cancel: &CancelToken,
    ) -> Sent {
        let config = self.stream.config();
        loop {
            if cancel.is_cancelled() {
                return Sent::Over;
            }
            let estimate = config.turn_estimate(conversation, &self.offered);
            let body = |max| config.turn_body(conversation, &self.offered, max);
            let result = tokio::select! {
                // Dropping the request drops the HTTP stream; the supervisor logs run.cancelled.
                biased;
                () = cancel.cancelled() => return Sent::Over,
                result = self.stream.turn(sink, &estimate, body, decoder) => result,
            };
            match result {
                Ok(()) => return Sent::Answered,
                Err(Failure::Ended) => return Sent::Over,
                Err(Failure::Emit(reason, message)) => {
                    fail_run(sink, reason, message);
                    return Sent::Over;
                }
                Err(Failure::SoftLimit(_)) => {
                    if self.runner.past_soft_limit(sink, cancel).await == BudgetAnswer::Stop {
                        return Sent::Over;
                    }
                }
            }
        }
    }
}
