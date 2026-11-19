//! Agents with tools: the agent loop (M3 item 3, docs/01 §4–5, TP-TOOL-07..12, 18).
//!
//! One loop drives any API that can carry tools ([`ToolProtocol`]): Grok over the Responses API
//! ([`ResponsesAgent`]) and Claude over the Messages API ([`MessagesAgent`]). A run is a series of
//! turns. Each turn is one request carrying the whole conversation and the agent's offered tools,
//! admitted by the budget meter like any request. When the model calls tools, each call goes
//! through the agent's [`ToolRunner`]: the policy decides, an Ask waits at a checkpoint on the
//! agent's card, and only an allowed call runs, in the gateway. What came back (the output, or why
//! it didn't run) goes into the next turn. The run completes when the model answers without
//! calling anything.
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
//! - **Provider-side tools** ([`ProviderTool`], M3 item 4) run on the provider's servers, where
//!   Corral can't pause them, so they're decided once, when the agent is built: an API offers one
//!   only if the agent's policy allows `provider_tools` ([`allows`]), and the default denies it.
//!   What such a tool reads marks the run as having read untrusted content (T28), before any of
//!   that turn's calls are decided. A turn the provider paused mid-way is sent back and resumed.

use crate::messages::MessagesConfig;
use crate::responses::ResponsesConfig;
use crate::stream::{Failure, Protocol, StreamDecoder, StreamDriver, fail_run};
use crate::transport::Transport;
use corral_core::budget::RequestEstimate;
use corral_core::domain::ActionClass;
use corral_core::event::FailureReason;
use corral_core::supervisor::{CancelToken, EventSink, RunDriver, RunFuture, RunRequest};
use corral_policy::{Action, Decision, Policy, RunGrants};
use corral_secrets::SecretStore;
use corral_tools::{
    BudgetAnswer, CheckpointDesk, IdSource, OfferedTool, ToolAnswer, ToolGateway, ToolRunner,
};
use serde_json::Value;
use std::sync::Arc;

/// The most model turns a run gets before Corral stops it.
pub const DEFAULT_MAX_TURNS: u32 = 20;

/// The most a single tool call's arguments can be, in bytes. A call over it fails the run.
pub const MAX_CALL_ARGUMENT_BYTES: usize = 1024 * 1024;

/// What the model is told when a call's arguments aren't a JSON object.
const BAD_ARGUMENTS: &str =
    "Corral couldn't read this call: its arguments must be a JSON object. Nothing ran.";

/// A tool call the model made: what to run, and the ID its result goes back under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionCall {
    /// Pairs the call with its output.
    pub call_id: String,
    /// The wire name of the tool it called.
    pub name: String,
    /// Its arguments, as the JSON text the model wrote.
    pub arguments: String,
}

/// A tool that runs on the provider's servers rather than in Corral.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderTool {
    /// Searching the web. Read-only, but what it finds is untrusted.
    WebSearch,
}

impl ProviderTool {
    /// The tool's name in the trace and the policy, e.g. `web_search`.
    pub fn name(self) -> &'static str {
        match self {
            Self::WebSearch => "web_search",
        }
    }
}

/// Whether `policy` lets an agent use `tool`. Only Allow does: the provider runs it without
/// asking Corral, so there's no moment to ask the user.
pub fn allows(policy: &Policy, tool: ProviderTool) -> bool {
    let action = Action::new(ActionClass::ProviderTools, tool.name());
    policy.decide(&action, &RunGrants::default()) == Decision::Allow
}

/// An API the agent loop can drive with tools: how a turn's request looks, how its calls are
/// read, and how a turn goes back into the conversation.
pub trait ToolProtocol: Protocol {
    /// Everything said so far in a run, in the API's own shape.
    type Conversation: Send + Sync;
    /// What the model said in a turn, as the conversation keeps it.
    type Said: Send;
    /// Reads one response, keeping its tool calls.
    type ToolDecoder: StreamDecoder + Send;
    /// A conversation that starts with the user's prompt.
    fn conversation(&self, prompt: &str) -> Self::Conversation;
    /// One turn's request: the conversation, the tools, the output limit the meter allowed.
    fn turn_body(
        &self,
        conversation: &Self::Conversation,
        tools: &[OfferedTool],
        max_output_tokens: u32,
    ) -> Value;
    /// The most one turn could cost.
    fn turn_estimate(
        &self,
        conversation: &Self::Conversation,
        tools: &[OfferedTool],
    ) -> RequestEstimate;
    /// A decoder for a response to a request that offered tools.
    fn tool_decoder(&self) -> Self::ToolDecoder;
    /// What the response said and the calls it made, once it's over.
    fn take_turn(decoder: &mut Self::ToolDecoder) -> (Self::Said, Vec<FunctionCall>);
    /// Adds a turn: what the model said, each call it made and what came back.
    fn record(
        conversation: &mut Self::Conversation,
        said: &Self::Said,
        results: &[(FunctionCall, String)],
    );
    /// The provider paused the turn part-way, so it goes back as it is to be resumed.
    fn paused(_decoder: &Self::ToolDecoder) -> bool {
        false
    }
    /// The untrusted content a provider-side tool read in the response, if any.
    fn untrusted(_decoder: &Self::ToolDecoder) -> Option<&'static str> {
        None
    }
    /// Offers only the provider-side tools `keep` says yes to.
    fn keep_provider_tools(&mut self, _keep: &dyn Fn(ProviderTool) -> bool) {}
}

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

/// A [`RunDriver`] for an agent with tools over any [`ToolProtocol`].
pub struct ToolAgent<P> {
    stream: StreamDriver<P>,
    tools: ToolSetup,
    max_turns: u32,
}

/// Grok with tools, over the Responses API.
pub type ResponsesAgent = ToolAgent<ResponsesConfig>;

/// Claude with tools, over the Messages API.
pub type MessagesAgent = ToolAgent<MessagesConfig>;

impl<P: ToolProtocol> ToolAgent<P> {
    /// An agent that sends through `transport`, reads keys from `keys`, and runs its tool calls
    /// through `tools`. Provider-side tools in `config` stay only if the policy allows them.
    pub fn new(
        transport: Arc<dyn Transport>,
        keys: Arc<dyn SecretStore>,
        mut config: P,
        tools: ToolSetup,
    ) -> Self {
        config.keep_provider_tools(&|tool| allows(&tools.policy, tool));
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

impl<P: ToolProtocol> RunDriver for ToolAgent<P> {
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
struct Agent<P> {
    stream: StreamDriver<P>,
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

impl<P: ToolProtocol> Agent<P> {
    async fn work(&self, prompt: &str, sink: &EventSink, cancel: &CancelToken) {
        let config = self.stream.config();
        let mut conversation = config.conversation(prompt);
        for _ in 0..self.max_turns {
            let mut decoder = config.tool_decoder();
            if let Sent::Over = self.send(&conversation, &mut decoder, sink, cancel).await {
                return;
            }
            let (said, calls) = P::take_turn(&mut decoder);
            if let Some(source) = P::untrusted(&decoder) {
                // Before this turn's calls are decided.
                self.runner.taint(source);
            }
            // No calls and not paused: the decoder logged run.completed (or run.failed).
            if calls.is_empty() && !P::paused(&decoder) {
                return;
            }
            let mut results = Vec::with_capacity(calls.len());
            for call in calls {
                let Ok(Value::Object(args)) = serde_json::from_str::<Value>(&call.arguments) else {
                    results.push((call, BAD_ARGUMENTS.to_owned()));
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
                results.push((call, output));
            }
            P::record(&mut conversation, &said, &results);
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
        conversation: &P::Conversation,
        decoder: &mut P::ToolDecoder,
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
