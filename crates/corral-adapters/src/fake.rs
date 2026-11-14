//! The FakeAdapter (M1 item 6, docs/04 §3): a scripted agent, so the core can be tested without
//! a model, a key or a network. A [`FakeScript`] says what the agent "does", step by step; a
//! [`FakeDriver`] runs it through the real supervisor, so every event goes through the same
//! lifecycle checks, budget meter and Stop path as a real provider's.
//!
//! - **Say** streams text (`model.delta`); the answer is everything said.
//! - **Wait** takes real (or paused-test) time and ends at once on Stop.
//! - **Stubborn** takes time and ignores Stop, like a misbehaving adapter, so the supervisor has
//!   to force-drop it (TP-SUP-02).
//! - **Tool** goes through the policy gate and the tool gateway when the driver has a
//!   [`ToolSetup`]: an Ask waits at a checkpoint, and the agent hears the answer and carries on.
//!   Without one the run fails with `tools_not_enabled` and nothing runs, exactly like the
//!   skeleton's real agents.
//! - **Fail** ends the run with a given reason.
//!
//! The eval runner builds these scripts from scenario files (M2).

use corral_core::budget::{Price, RequestEstimate, input_token_bound};
use corral_core::event::{
    EventBody, FailureReason, ModelDelta, ModelRequest, ModelResponse, RunCompleted, RunFailed,
    UsageReported,
};
use corral_core::supervisor::{CancelToken, EventSink, RunDriver, RunFuture, RunRequest};
use corral_policy::Policy;
use corral_tools::{CheckpointDesk, IdSource, ToolAnswer, ToolGateway, ToolRunner};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The output ceiling a fake request asks the budget meter for.
const FAKE_MAX_OUTPUT_TOKENS: u32 = 1024;

/// One thing a scripted agent does.
#[derive(Debug, Clone, PartialEq)]
pub enum FakeStep {
    /// Streams this text.
    Say(String),
    /// Waits this long; Stop ends the wait at once.
    Wait(Duration),
    /// Adds to the usage reported at the end.
    Usage {
        /// Input tokens.
        input_tokens: u64,
        /// Output tokens.
        output_tokens: u64,
        /// Cost in micro-USD.
        cost_micro_usd: u64,
    },
    /// Asks to use a tool: through the gate with a [`ToolSetup`], refused without one.
    Tool {
        /// The tool, e.g. `email.send`.
        name: String,
        /// Its arguments.
        args: serde_json::Value,
    },
    /// Ends the run as failed.
    Fail {
        /// Why.
        reason: FailureReason,
        /// What the card shows.
        message: String,
    },
    /// Takes this long and ignores Stop.
    Stubborn(Duration),
}

/// What a scripted agent does, in order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FakeScript {
    steps: Vec<FakeStep>,
}

impl FakeScript {
    /// Adds a step.
    pub fn step(mut self, step: FakeStep) -> Self {
        self.steps.push(step);
        self
    }

    /// Streams `text`.
    pub fn say(self, text: impl Into<String>) -> Self {
        self.step(FakeStep::Say(text.into()))
    }

    /// Waits; Stop ends the wait.
    pub fn wait(self, duration: Duration) -> Self {
        self.step(FakeStep::Wait(duration))
    }

    /// Adds usage.
    pub fn usage(self, input_tokens: u64, output_tokens: u64, cost_micro_usd: u64) -> Self {
        self.step(FakeStep::Usage {
            input_tokens,
            output_tokens,
            cost_micro_usd,
        })
    }

    /// Asks to use a tool.
    pub fn tool(self, name: impl Into<String>, args: serde_json::Value) -> Self {
        self.step(FakeStep::Tool {
            name: name.into(),
            args,
        })
    }

    /// Fails the run.
    pub fn fail(self, reason: FailureReason, message: impl Into<String>) -> Self {
        self.step(FakeStep::Fail {
            reason,
            message: message.into(),
        })
    }

    /// Takes time and ignores Stop.
    pub fn stubborn(self, duration: Duration) -> Self {
        self.step(FakeStep::Stubborn(duration))
    }

    /// The steps, in order.
    pub fn steps(&self) -> &[FakeStep] {
        &self.steps
    }
}

/// What a fake agent's tool calls go through: the same policy, gateway and desk as a real one.
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

/// Runs scripts. Each run gets the script for its prompt, or the default one.
pub struct FakeDriver {
    default: Arc<FakeScript>,
    by_prompt: HashMap<String, Arc<FakeScript>>,
    price: Price,
    runs: Arc<Mutex<Vec<RunRequest>>>,
    tools: Option<ToolSetup>,
    answers: Arc<Mutex<Vec<ToolAnswer>>>,
}

impl FakeDriver {
    /// A driver that runs `script` for every prompt, free of charge.
    pub fn new(script: FakeScript) -> Self {
        Self {
            default: Arc::new(script),
            by_prompt: HashMap::new(),
            price: Price::per_million_tokens(0, 0),
            runs: Arc::default(),
            tools: None,
            answers: Arc::default(),
        }
    }

    /// Runs `script` for runs whose prompt is exactly `prompt`.
    pub fn with_prompt(mut self, prompt: impl Into<String>, script: FakeScript) -> Self {
        self.by_prompt.insert(prompt.into(), Arc::new(script));
        self
    }

    /// Charges `price` at the budget meter, like a real model.
    pub fn priced(mut self, price: Price) -> Self {
        self.price = price;
        self
    }

    /// Sends tool steps through `setup` instead of refusing them.
    pub fn with_tools(mut self, setup: ToolSetup) -> Self {
        self.tools = Some(setup);
        self
    }

    /// What the agent was told about each tool call, in order.
    pub fn tool_answers(&self) -> Arc<Mutex<Vec<ToolAnswer>>> {
        Arc::clone(&self.answers)
    }

    /// Every run this driver was given, in order.
    pub fn runs(&self) -> Arc<Mutex<Vec<RunRequest>>> {
        Arc::clone(&self.runs)
    }
}

impl RunDriver for FakeDriver {
    fn run(&self, request: RunRequest, sink: EventSink, cancel: CancelToken) -> RunFuture {
        let script = self
            .by_prompt
            .get(&request.prompt)
            .unwrap_or(&self.default)
            .clone();
        let estimate = RequestEstimate {
            price: self.price,
            input_tokens: input_token_bound(&request.prompt),
            tool_fees_micro_usd: 0,
            max_output_tokens: FAKE_MAX_OUTPUT_TOKENS,
        };
        if let Ok(mut runs) = self.runs.lock() {
            runs.push(request);
        }
        let tools = self.tools.clone().map(|setup| Tools {
            runner: ToolRunner::new(setup.policy, setup.gateway, setup.desk, setup.ids),
            answers: Arc::clone(&self.answers),
        });
        Box::pin(async move { play(&script, &sink, &cancel, &estimate, tools.as_ref()).await })
    }
}

/// One run's tools.
struct Tools {
    runner: ToolRunner,
    answers: Arc<Mutex<Vec<ToolAnswer>>>,
}

/// Plays a script. Returning without a terminal event leaves the run to the supervisor: it logs
/// `run.cancelled` after Stop, or fails the run as an adapter error.
async fn play(
    script: &FakeScript,
    sink: &EventSink,
    cancel: &CancelToken,
    estimate: &RequestEstimate,
    tools: Option<&Tools>,
) {
    let request = ModelRequest {
        provider: "fake".into(),
        model: "fake-1".into(),
    };
    // On a refusal the core has already ended the run (budget_hard_cap); nothing is "sent".
    if sink.request_model(request, estimate).is_err() {
        return;
    }
    let mut text = String::new();
    let (mut input, mut output, mut cost) = (0_u64, 0_u64, 0_u64);
    for step in script.steps() {
        if cancel.is_cancelled() {
            return;
        }
        match step {
            FakeStep::Say(said) => {
                if !emit(
                    sink,
                    EventBody::ModelDelta(ModelDelta { text: said.clone() }),
                ) {
                    return;
                }
                text.push_str(said);
            }
            FakeStep::Wait(duration) => {
                tokio::select! {
                    () = tokio::time::sleep(*duration) => {}
                    () = cancel.cancelled() => return,
                }
            }
            FakeStep::Usage {
                input_tokens,
                output_tokens,
                cost_micro_usd,
            } => {
                input = input.saturating_add(*input_tokens);
                output = output.saturating_add(*output_tokens);
                cost = cost.saturating_add(*cost_micro_usd);
            }
            FakeStep::Tool { name, args } if let Some(tools) = tools => {
                let answer = tools.runner.call(name, args.clone(), sink, cancel).await;
                let stopped = answer == ToolAnswer::Stopped;
                if let Ok(mut answers) = tools.answers.lock() {
                    answers.push(answer);
                }
                if stopped {
                    return;
                }
            }
            FakeStep::Tool { name, args: _ } => {
                emit(
                    sink,
                    EventBody::RunFailed(RunFailed {
                        reason: FailureReason::ToolsNotEnabled,
                        message: format!(
                            "The agent asked to use a tool ({name}), but this agent has no tools yet. Nothing ran."
                        ),
                    }),
                );
                return;
            }
            FakeStep::Fail { reason, message } => {
                emit(
                    sink,
                    EventBody::RunFailed(RunFailed {
                        reason: *reason,
                        message: message.clone(),
                    }),
                );
                return;
            }
            FakeStep::Stubborn(duration) => tokio::time::sleep(*duration).await,
        }
    }
    let finished = emit(
        sink,
        EventBody::ModelResponse(ModelResponse {
            text,
            finish_reason: Some("stop".into()),
        }),
    ) && emit(
        sink,
        EventBody::UsageReported(UsageReported {
            input_tokens: input,
            output_tokens: output,
            cost_micro_usd: cost,
        }),
    );
    if finished {
        emit(
            sink,
            EventBody::RunCompleted(RunCompleted { summary: None }),
        );
    }
}

/// Emits one event. A refusal means the run has ended (cancelled, or an event the lifecycle
/// refused); the script stops and the supervisor closes the run.
fn emit(sink: &EventSink, body: EventBody) -> bool {
    sink.emit(body).is_ok()
}
