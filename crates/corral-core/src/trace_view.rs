//! The trace viewer's span tree, projected from one run's events (M4 item 5, docs/01 §7,
//! TP-TV-01..09). The log is the only source (ADR-002); spans are derived here, named the way
//! OpenTelemetry's GenAI conventions name them:
//!
//! - **`run`**, "invoke_agent <agent>": from `run.queued` to the run's end, with the prompt, the
//!   adapter and the run's total tokens and cost.
//! - **`model-<n>`**, "chat <model>": from each `model.request` to its `model.response`, with the
//!   provider, the finish reason, the response, and the usage reported after it.
//! - **`tool-<call>`**, "execute_tool <tool>": from `tool.requested` to `tool.result`, beside the
//!   model calls, with the class, the arguments, the policy's decision and the result. A call the
//!   policy denied ends at the decision.
//! - **`checkpoint-<id>`**, "checkpoint: <title>", under its tool call: from opened to resolved,
//!   with the risk, the detail and the resolution.
//! - **Marks**: budget warnings, pauses, resumes and plan updates, zero-length on the run.
//!
//! When a run fails or stops, every span still open ends with it. Model text, arguments, results
//! and checkpoint details are agent text: cleaned and capped at [`MAX_ATTRIBUTE_CHARS`].

use crate::agent_text::AgentText;
use crate::budget::usd;
use crate::domain::{ActionClass, Risk};
use crate::event::{
    BudgetScope, Event, EventBody, PauseReason, PolicyDecision, Resolution, StepStatus,
};
use serde::Serialize;
use std::collections::HashMap;

/// The most characters an attribute's value keeps.
pub const MAX_ATTRIBUTE_CHARS: usize = 2_000;

/// One run's spans, in the order they began.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TraceView {
    /// The run.
    pub run_id: String,
    /// When its first event was logged, in ms since the epoch.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub start_ms: u64,
    /// When it ended, or none while it's going.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub end_ms: Option<u64>,
    /// Its spans.
    pub spans: Vec<Span>,
}

/// One step of a run, with its time and what happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Span {
    /// Unique in the run: `run`, `model-1`, `tool-<call>`, `checkpoint-<id>`, `mark-<n>`.
    pub id: String,
    /// The span it's under, or none for the run.
    pub parent: Option<String>,
    /// What it is.
    pub kind: SpanKind,
    /// Its name, e.g. "chat claude-sonnet-5-5".
    pub name: String,
    /// When it began, in ms since the epoch.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub start_ms: u64,
    /// When it ended, or none while it's open.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub end_ms: Option<u64>,
    /// How it went.
    pub status: SpanStatus,
    /// What the side panel shows, in order.
    pub attributes: Vec<Attribute>,
}

/// One labelled value of a span.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Attribute {
    /// The label, e.g. "input tokens".
    pub key: String,
    /// The value, as text.
    pub value: String,
}

/// What a span is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum SpanKind {
    /// The whole run.
    Run,
    /// One model call.
    Model,
    /// One tool call.
    Tool,
    /// A checkpoint on a tool call.
    Checkpoint,
    /// A moment on the run: a budget warning, a pause, a resume, a plan update.
    Mark,
}

/// How a span went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum SpanStatus {
    /// Still open.
    Running,
    /// Finished well.
    Ok,
    /// Failed.
    Error,
    /// The policy or the user said no.
    Denied,
    /// Stopped, or a checkpoint expired.
    Cancelled,
}

/// The span tree of one run's events, in log order.
pub fn trace_view(events: &[Event]) -> TraceView {
    let mut tree = Tree::default();
    for event in events {
        tree.observe(event);
    }
    TraceView {
        run_id: events
            .first()
            .map(|e| e.run_id.as_str().to_owned())
            .unwrap_or_default(),
        start_ms: events.first().map(|e| e.ts).unwrap_or_default(),
        end_ms: tree.end_ms,
        spans: tree.spans,
    }
}

/// Agent text, cleaned and capped.
fn text(raw: &str) -> String {
    AgentText::new(raw, MAX_ATTRIBUTE_CHARS).as_str().to_owned()
}

/// A JSON value as one line of agent text.
fn json_text(value: &serde_json::Value) -> String {
    text(&value.to_string())
}

/// An action class by its policy name, e.g. `write.local`.
fn class_name(class: ActionClass) -> String {
    match serde_json::to_value(class) {
        Ok(serde_json::Value::String(name)) => name,
        // Every class serializes to its name; this keeps the view whole if one ever didn't.
        Ok(_) | Err(_) => format!("{class:?}"),
    }
}

fn risk_name(risk: Risk) -> &'static str {
    match risk {
        Risk::Normal => "normal",
        Risk::High => "high",
        Risk::Critical => "critical",
    }
}

fn step_name(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => "pending",
        StepStatus::InProgress => "in progress",
        StepStatus::Done => "done",
        StepStatus::Skipped => "skipped",
    }
}

#[derive(Default)]
struct Tree {
    spans: Vec<Span>,
    by_id: HashMap<String, usize>,
    models: u32,
    marks: u32,
    /// The model call waiting for its response.
    open_model: Option<usize>,
    /// The latest model call, which usage after a response belongs to.
    last_model: Option<usize>,
    input_tokens: u64,
    output_tokens: u64,
    cost_micro_usd: u64,
    end_ms: Option<u64>,
}

impl Tree {
    fn push(&mut self, span: Span) -> usize {
        let index = self.spans.len();
        self.by_id.insert(span.id.clone(), index);
        self.spans.push(span);
        index
    }

    /// The run span, made from the first event if the log has no `run.queued`.
    fn run(&mut self, ts: u64) -> usize {
        if let Some(index) = self.by_id.get("run") {
            return *index;
        }
        self.push(open(
            "run".into(),
            None,
            SpanKind::Run,
            "invoke_agent ?".into(),
            ts,
        ))
    }

    fn set(&mut self, index: usize, key: &str, value: String) {
        let attributes = &mut self.spans[index].attributes;
        if let Some(existing) = attributes.iter_mut().find(|a| a.key == key) {
            existing.value = value;
        } else {
            attributes.push(Attribute {
                key: key.to_owned(),
                value,
            });
        }
    }

    fn close(&mut self, index: usize, ts: u64, status: SpanStatus) {
        let span = &mut self.spans[index];
        span.end_ms = Some(ts);
        span.status = status;
    }

    fn mark(&mut self, ts: u64, name: String) -> usize {
        self.run(ts);
        self.marks += 1;
        let id = format!("mark-{}", self.marks);
        let mut span = open(id, Some("run".into()), SpanKind::Mark, name, ts);
        span.end_ms = Some(ts);
        span.status = SpanStatus::Ok;
        self.push(span)
    }

    /// The tool call's span, made where its result is if no request was logged.
    fn tool(&mut self, call: &str, ts: u64) -> usize {
        let id = format!("tool-{call}");
        if let Some(index) = self.by_id.get(&id) {
            return *index;
        }
        self.run(ts);
        self.push(open(
            id,
            Some("run".into()),
            SpanKind::Tool,
            "execute_tool ?".into(),
            ts,
        ))
    }

    /// Ends the run and every span still open in it.
    fn end(&mut self, ts: u64, status: SpanStatus) -> usize {
        let run = self.run(ts);
        for span in &mut self.spans {
            if span.end_ms.is_none() {
                span.end_ms = Some(ts);
                span.status = status;
            }
        }
        self.open_model = None;
        self.end_ms = Some(ts);
        run
    }

    fn observe(&mut self, event: &Event) {
        let ts = event.ts;
        match &event.body {
            EventBody::RunQueued(queued) => {
                let run = self.run(ts);
                self.spans[run].name = format!("invoke_agent {}", text(queued.agent_id.as_str()));
                self.spans[run].start_ms = ts;
                self.set(run, "prompt", text(&queued.prompt));
            }
            EventBody::RunStarted(started) => {
                let run = self.run(ts);
                self.set(run, "adapter", text(&started.adapter));
            }
            EventBody::PlanUpdated(plan) => {
                let done = plan
                    .steps
                    .iter()
                    .filter(|s| s.status == StepStatus::Done)
                    .count();
                let mark = self.mark(ts, format!("plan: {done} of {} done", plan.steps.len()));
                let steps: Vec<String> = plan
                    .steps
                    .iter()
                    .map(|s| format!("{} ({})", text(&s.title), step_name(s.status)))
                    .collect();
                self.set(mark, "steps", text(&steps.join("\n")));
            }
            EventBody::ModelRequest(request) => {
                self.run(ts);
                self.models += 1;
                let id = format!("model-{}", self.models);
                let model = self.push(open(
                    id,
                    Some("run".into()),
                    SpanKind::Model,
                    format!("chat {}", text(&request.model)),
                    ts,
                ));
                self.set(model, "provider", text(&request.provider));
                self.set(model, "model", text(&request.model));
                self.open_model = Some(model);
                self.last_model = Some(model);
            }
            EventBody::ModelDelta(_) | EventBody::ToolStarted(_) | EventBody::SessionClosed(_) => {}
            EventBody::ModelResponse(response) => {
                let Some(model) = self.open_model.take() else {
                    return;
                };
                self.close(model, ts, SpanStatus::Ok);
                if let Some(reason) = &response.finish_reason {
                    self.set(model, "finish reason", text(reason));
                }
                self.set(model, "response", text(&response.text));
            }
            EventBody::UsageReported(usage) => {
                self.input_tokens = self.input_tokens.saturating_add(usage.input_tokens);
                self.output_tokens = self.output_tokens.saturating_add(usage.output_tokens);
                self.cost_micro_usd = self.cost_micro_usd.saturating_add(usage.cost_micro_usd);
                if let Some(model) = self.last_model {
                    self.set(model, "input tokens", usage.input_tokens.to_string());
                    self.set(model, "output tokens", usage.output_tokens.to_string());
                    self.set(model, "cost", usd(usage.cost_micro_usd));
                }
                let run = self.run(ts);
                self.set(run, "input tokens", self.input_tokens.to_string());
                self.set(run, "output tokens", self.output_tokens.to_string());
                self.set(run, "cost", usd(self.cost_micro_usd));
            }
            EventBody::ToolRequested(requested) => {
                let call = self.tool(requested.tool_call_id.as_str(), ts);
                self.spans[call].name = format!("execute_tool {}", text(&requested.tool));
                self.spans[call].start_ms = ts;
                self.set(call, "class", class_name(requested.action_class));
                self.set(call, "arguments", json_text(&requested.args));
            }
            EventBody::PolicyDecided(decided) => {
                let call = self.tool(decided.tool_call_id.as_str(), ts);
                let decision = match &decided.decision {
                    PolicyDecision::Allow => "allow".to_owned(),
                    PolicyDecision::Ask { risk, reason } => {
                        format!("ask ({}): {}", risk_name(*risk), text(reason))
                    }
                    PolicyDecision::Deny { reason } => {
                        self.close(call, ts, SpanStatus::Denied);
                        format!("deny: {}", text(reason))
                    }
                };
                self.set(call, "decision", decision);
            }
            EventBody::CheckpointOpened(opened) => {
                let call = self.tool(opened.tool_call_id.as_str(), ts);
                let parent = self.spans[call].id.clone();
                let checkpoint = self.push(open(
                    format!("checkpoint-{}", opened.checkpoint_id.as_str()),
                    Some(parent),
                    SpanKind::Checkpoint,
                    format!("checkpoint: {}", text(&opened.title)),
                    ts,
                ));
                self.set(checkpoint, "risk", risk_name(opened.risk).to_owned());
                self.set(checkpoint, "detail", text(&opened.detail));
            }
            EventBody::CheckpointResolved(resolved) => {
                let id = format!("checkpoint-{}", resolved.checkpoint_id.as_str());
                let Some(checkpoint) = self.by_id.get(&id).copied() else {
                    return;
                };
                let (status, resolution) = match &resolved.resolution {
                    Resolution::Approved => (SpanStatus::Ok, "approved".to_owned()),
                    Resolution::Denied { note: Some(note) } => {
                        (SpanStatus::Denied, format!("denied: {}", text(note)))
                    }
                    Resolution::Denied { note: None } => (SpanStatus::Denied, "denied".to_owned()),
                    Resolution::Edited { .. } => (SpanStatus::Ok, "edited".to_owned()),
                    Resolution::AllowedForRun => (SpanStatus::Ok, "allowed for the run".to_owned()),
                    Resolution::Expired => (SpanStatus::Cancelled, "expired".to_owned()),
                };
                self.close(checkpoint, ts, status);
                self.set(checkpoint, "resolution", resolution);
            }
            EventBody::ToolResult(result) => {
                let call = self.tool(result.tool_call_id.as_str(), ts);
                let status = if result.ok {
                    SpanStatus::Ok
                } else {
                    SpanStatus::Error
                };
                self.close(call, ts, status);
                self.set(call, "result", json_text(&result.output));
            }
            EventBody::BudgetWarned(warned) => {
                let scope = match warned.scope {
                    BudgetScope::Run => "for the run",
                    BudgetScope::AgentMonth => "for the agent this month",
                    BudgetScope::Global => "in total",
                };
                self.mark(
                    ts,
                    format!(
                        "budget warning: {} of {} {scope}",
                        usd(warned.spent_micro_usd),
                        usd(warned.limit_micro_usd)
                    ),
                );
            }
            EventBody::RunPaused(paused) => {
                let reason = match paused.reason {
                    PauseReason::User => "by you",
                    PauseReason::Budget => "budget",
                };
                self.mark(ts, format!("paused: {reason}"));
            }
            EventBody::RunResumed(_) => {
                self.mark(ts, "resumed".into());
            }
            EventBody::RunCompleted(done) => {
                let run = self.end(ts, SpanStatus::Ok);
                if let Some(summary) = &done.summary {
                    self.set(run, "summary", text(summary));
                }
            }
            EventBody::RunFailed(failed) => {
                let run = self.end(ts, SpanStatus::Error);
                self.set(run, "error", text(&failed.message));
            }
            EventBody::RunCancelled(_) => {
                self.end(ts, SpanStatus::Cancelled);
            }
        }
    }
}

/// A span that began at `ts` and is still open.
fn open(id: String, parent: Option<String>, kind: SpanKind, name: String, ts: u64) -> Span {
    Span {
        id,
        parent,
        kind,
        name,
        start_ms: ts,
        end_ms: None,
        status: SpanStatus::Running,
        attributes: Vec::new(),
    }
}
