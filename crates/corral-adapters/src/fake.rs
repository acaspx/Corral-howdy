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

use corral_core::budget::{Admission, BudgetError, Price, RequestEstimate, input_token_bound};
use corral_core::event::{
    EventBody, FailureReason, ModelDelta, ModelRequest, ModelResponse, RunCompleted, RunFailed,
    UsageReported,
};
use corral_core::ids::AgentId;
use corral_core::supervisor::{
    CancelToken, EventSink, RunDriver, RunFuture, RunRequest, SupervisorError,
};
use corral_tools::{BudgetAnswer, ToolAnswer, ToolRunner};
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
    /// Adds usage the way an honest provider bills it: output stops at the turn's admitted
    /// max output tokens, and the cost is tokens × the fake's price. An explicit
    /// [`FakeStep::Usage`] is reported as given instead, like a provider that overbills.
    Billed {
        /// Input tokens.
        input_tokens: u64,
        /// Output tokens the model would write, before the request's limit.
        output_tokens: u64,
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
    /// Ends this model turn (its response and usage) and asks the model again, through the
    /// budget meter: at the soft limit, the run waits for the user (TP-BUD-02).
    Request,
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

    /// Adds usage billed like an honest provider: see [`FakeStep::Billed`].
    pub fn billed(self, input_tokens: u64, output_tokens: u64) -> Self {
        self.step(FakeStep::Billed {
            input_tokens,
            output_tokens,
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

    /// Ends this model turn and asks the model again.
    pub fn request(self) -> Self {
        self.step(FakeStep::Request)
    }

    /// The steps, in order.
    pub fn steps(&self) -> &[FakeStep] {
        &self.steps
    }
}

/// What a fake agent's tool calls go through: the same policy, gateway and desk as a real one.
pub use crate::agent::ToolSetup;

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
    /// The script stops at a safe point before each step.
    fn pauses(&self, _agent: &AgentId) -> bool {
        true
    }

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
    let Some(mut admitted) = ask_model(sink, cancel, estimate, tools).await else {
        return;
    };
    let mut text = String::new();
    let mut turn = Turn::default();
    for step in script.steps() {
        // A safe point: a pause asked for takes hold here, between steps.
        if !sink.pause_point(cancel).await {
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
                turn.input = turn.input.saturating_add(*input_tokens);
                turn.output = turn.output.saturating_add(*output_tokens);
                turn.cost = turn.cost.saturating_add(*cost_micro_usd);
            }
            FakeStep::Billed {
                input_tokens,
                output_tokens,
            } => {
                turn.billed_input = turn.billed_input.saturating_add(*input_tokens);
                turn.billed_output = turn.billed_output.saturating_add(*output_tokens);
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
            FakeStep::Request => {
                let ended = emit(
                    sink,
                    EventBody::ModelResponse(ModelResponse {
                        text: std::mem::take(&mut text),
                        finish_reason: Some("tool_calls".into()),
                    }),
                ) && emit(
                    sink,
                    EventBody::UsageReported(
                        std::mem::take(&mut turn).usage(&admitted, estimate.price),
                    ),
                );
                if !ended {
                    return;
                }
                let Some(next) = ask_model(sink, cancel, estimate, tools).await else {
                    return;
                };
                admitted = next;
            }
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
        EventBody::UsageReported(turn.usage(&admitted, estimate.price)),
    );
    if finished {
        emit(
            sink,
            EventBody::RunCompleted(RunCompleted { summary: None }),
        );
    }
}

/// One model turn's usage so far.
#[derive(Debug, Default)]
struct Turn {
    input: u64,
    output: u64,
    cost: u64,
    billed_input: u64,
    billed_output: u64,
}

impl Turn {
    /// What the turn reports: scripted usage as given, plus billed usage as an honest provider
    /// bills it, its output stopped at the request's limit and priced at `price`.
    fn usage(self, admitted: &Admission, price: Price) -> UsageReported {
        let billed_output = self
            .billed_output
            .min(u64::from(admitted.max_output_tokens));
        let billed = price.cost(self.billed_input, billed_output);
        UsageReported {
            input_tokens: self.input.saturating_add(self.billed_input),
            output_tokens: self.output.saturating_add(billed_output),
            cost_micro_usd: self.cost.saturating_add(billed),
        }
    }
}

/// Asks the budget meter for a model request: what it admitted, or `None` when the run has ended
/// or must stop.
///
/// At the soft limit an agent with tools asks the user (a `budget.threshold` checkpoint) and
/// tries again if they approve; one without can't ask, so it stops there. On any other refusal
/// the core has already ended the run (`budget_hard_cap`); nothing is "sent".
async fn ask_model(
    sink: &EventSink,
    cancel: &CancelToken,
    estimate: &RequestEstimate,
    tools: Option<&Tools>,
) -> Option<Admission> {
    let mut asked = false;
    loop {
        let request = ModelRequest {
            provider: "fake".into(),
            model: "fake-1".into(),
        };
        match sink.request_model(request, estimate) {
            Ok(admitted) => return Some(admitted),
            Err(SupervisorError::Budget(refusal @ BudgetError::SoftLimit { .. })) => {
                // Asked once and still held: something's wrong, so stop rather than ask forever.
                let tools = tools.filter(|_| !asked);
                asked = true;
                let Some(tools) = tools else {
                    emit(
                        sink,
                        EventBody::RunFailed(RunFailed {
                            reason: FailureReason::BudgetHardCap,
                            message: refusal.to_string(),
                        }),
                    );
                    return None;
                };
                if tools.runner.past_soft_limit(sink, cancel).await == BudgetAnswer::Stop {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
}

/// Emits one event. A refusal means the run has ended (cancelled, or an event the lifecycle
/// refused); the script stops and the supervisor closes the run.
fn emit(sink: &EventSink, body: EventBody) -> bool {
    sink.emit(body).is_ok()
}
