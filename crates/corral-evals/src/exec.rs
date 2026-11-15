//! Running scenarios on the FakeAdapter (M2 item 8, docs/05-evals.md).
//!
//! Each scenario's steps become a [`FakeScript`] that runs through the real supervisor, policy,
//! gate, checkpoint desk and tool gateway, under the profile the scenario names. The runner then
//! checks the event log against `expect`:
//!
//! - **The user.** The runner plays a user who approves every checkpoint the scenario doesn't
//!   name and denies the one it names (`expect.checkpoint.class`). A dangerous action has to be
//!   caught even when everything else was waved through.
//! - **The tools** are a fixed catalog ([`CATALOG`]) with the classes a real adapter would give
//!   them. A tool the catalog doesn't have isn't registered, so its class is `unknown` and the
//!   policy denies it. A tool step's `returns_fixture` is what the tool returns; fixtures are read
//!   only from the `fixtures/` folder next to the scenario.
//! - **Fails closed.** An unknown profile, an expectation the runner can't check, a bad fixture or
//!   a run that doesn't end is a failure with its reason. An adapter the runner can't drive is
//!   pending, and a pending safety scenario fails the gate.
//! - **Built-in check:** every `tool.started` follows a `policy.decided` for the same call
//!   (TP-ADP-02).
//!
//! Time is tokio's paused clock, so a checkpoint timeout of 30 minutes passes instantly.

use crate::{CheckpointExpect, Expect, NeverRule, Risk, Scenario, ScenarioResult, Status};
use corral_adapters::fake::{FakeDriver, FakeScript, ToolSetup};
use corral_core::budget::BudgetConfig;
use corral_core::clock::ManualClock;
use corral_core::domain::{self, ActionClass};
use corral_core::event::{CheckpointOpened, Event, EventBody, Resolution};
use corral_core::ids::{AgentId, RunId, SessionId, ToolCallId};
use corral_core::lifecycle::{Lifecycle, next};
use corral_core::supervisor::{EventLog, LogError, RunRequest, Supervisor, SupervisorConfig};
use corral_policy::{Approval, Policy, Profile, Scope};
use corral_tools::{Answer, BoxFuture, CheckpointDesk, Tool, ToolError, ToolGateway};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// The largest fixture the runner reads.
pub const MAX_FIXTURE_BYTES: u64 = 1_048_576;

/// How long a run may take on the paused clock (a week) before the runner gives up on it.
const RUN_LIMIT: Duration = Duration::from_secs(604_800);

/// The note the runner's user gives when it denies.
const DENY_NOTE: &str = "Denied by the eval runner.";

/// The tools scenarios can call: name, class, whether what it returns is untrusted, and the
/// argument that names its destination.
pub const CATALOG: [(&str, ActionClass, bool, Option<&str>); 8] = [
    ("http.get", ActionClass::NetReadUnlisted, true, Some("url")),
    ("http.post", ActionClass::NetWrite, false, Some("url")),
    ("email.send", ActionClass::CommsSend, false, Some("to")),
    ("fs.read", ActionClass::ReadLocal, false, Some("path")),
    ("fs.write", ActionClass::WriteLocal, false, Some("path")),
    ("fs.delete", ActionClass::DeleteLocal, false, Some("path")),
    ("shell.exec", ActionClass::ExecShell, false, None),
    ("payment.send", ActionClass::MoneySpend, false, Some("to")),
];

/// The state names `if_denied.run_state` can name.
const RUN_STATES: [&str; 8] = [
    "Idle",
    "Queued",
    "Running",
    "Paused",
    "AwaitingCheckpoint",
    "Completed",
    "Failed",
    "Cancelled",
];

/// Policy profiles scenarios can name.
#[derive(Debug, Clone)]
pub struct Profiles(BTreeMap<String, Profile>);

impl Profiles {
    /// The profiles every scenario can use: `default`, the deny-by-default profile an agent
    /// gets unless its manifest says otherwise.
    pub fn builtin() -> Self {
        Self(BTreeMap::from([("default".to_owned(), Profile::default())]))
    }

    /// Adds (or replaces) a profile, for tests.
    pub fn with(mut self, name: impl Into<String>, profile: Profile) -> Self {
        self.0.insert(name.into(), profile);
        self
    }
}

/// One scenario's result, why it isn't a pass, and the events the run logged.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// Passed, failed or pending.
    pub result: ScenarioResult,
    /// Why it failed or is pending, in plain words.
    pub reason: Option<String>,
    /// Everything the run logged, in order.
    pub events: Vec<Event>,
}

/// Runs every scenario, in ID order.
pub fn execute_all(scenarios: &[Scenario], profiles: &Profiles) -> Vec<Outcome> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build();
    let mut outcomes: Vec<Outcome> = match runtime {
        Ok(runtime) => scenarios
            .iter()
            .map(|s| runtime.block_on(execute(s, profiles)))
            .collect(),
        Err(e) => scenarios
            .iter()
            .map(|s| {
                outcome(
                    s,
                    Status::Failed,
                    Some(format!("the runner couldn't start: {e}")),
                    Vec::new(),
                )
            })
            .collect(),
    };
    outcomes.sort_by(|a, b| a.result.id.cmp(&b.result.id));
    outcomes
}

fn outcome(
    scenario: &Scenario,
    status: Status,
    reason: Option<String>,
    events: Vec<Event>,
) -> Outcome {
    Outcome {
        result: ScenarioResult {
            id: scenario.id.clone(),
            title: scenario.title.clone(),
            status,
        },
        reason,
        events,
    }
}

async fn execute(scenario: &Scenario, profiles: &Profiles) -> Outcome {
    if scenario.agent.adapter != "fake" {
        let reason = format!(
            "the runner can drive only the fake adapter; this scenario uses `{}`",
            scenario.agent.adapter
        );
        return outcome(scenario, Status::Pending, Some(reason), Vec::new());
    }
    match prepare(scenario, profiles) {
        Err(reason) => outcome(scenario, Status::Failed, Some(reason), Vec::new()),
        Ok(plan) => {
            let (events, mut problems) = play(scenario, plan).await;
            problems.extend(check(&scenario.expect, &events));
            if problems.is_empty() {
                outcome(scenario, Status::Passed, None, events)
            } else {
                outcome(scenario, Status::Failed, Some(problems.join("; ")), events)
            }
        }
    }
}

/// What a scenario runs with, checked before anything runs.
struct Plan {
    profile: Profile,
    script: FakeScript,
    outputs: HashMap<String, VecDeque<String>>,
    deny: Option<ActionClass>,
}

fn prepare(scenario: &Scenario, profiles: &Profiles) -> Result<Plan, String> {
    let profile = profiles
        .0
        .get(&scenario.agent.policy_profile)
        .cloned()
        .ok_or_else(|| format!("unknown policy profile `{}`", scenario.agent.policy_profile))?;
    supported(&scenario.expect)?;
    let deny = scenario
        .expect
        .checkpoint
        .as_ref()
        .map(|c| class(&c.class))
        .transpose()?;
    let mut script = FakeScript::default();
    let mut outputs: HashMap<String, VecDeque<String>> = HashMap::new();
    for (i, step) in scenario.steps.iter().enumerate() {
        let n = i + 1;
        match (&step.tool, &step.model_says) {
            (Some(tool), _) => {
                let args = match &step.args {
                    Some(args) => serde_json::to_value(args)
                        .map_err(|e| format!("step {n}: its arguments aren't JSON: {e}"))?,
                    None => json!({}),
                };
                let output = match &step.returns_fixture {
                    Some(fixture) => read_fixture(&scenario.dir, fixture)?,
                    None => format!("{tool}: done"),
                };
                outputs.entry(tool.clone()).or_default().push_back(output);
                script = script.tool(tool.clone(), args);
            }
            (None, Some(text)) => script = script.say(text.clone()),
            // parse_scenario rejects a step that does neither.
            (None, None) => return Err(format!("step {n} does nothing")),
        }
    }
    Ok(Plan {
        profile,
        script,
        outputs,
        deny,
    })
}

/// Fails on an expectation the runner can't check, rather than passing it unchecked.
fn supported(expect: &Expect) -> Result<(), String> {
    for rule in &expect.never {
        if rule.event != "tool.started" {
            return Err(format!(
                "the runner can't check `never` on event `{}`",
                rule.event
            ));
        }
        if let Some(without) = &rule.without
            && without != "checkpoint.resolved.approved"
        {
            return Err(format!(
                "the runner can't check `never ... without: {without}`"
            ));
        }
    }
    if let Some(state) = expect
        .if_denied
        .as_ref()
        .and_then(|d| d.run_state.as_deref())
        && !RUN_STATES.contains(&state)
    {
        return Err(format!("the runner can't check run_state `{state}`"));
    }
    if expect.if_denied.is_some() && expect.checkpoint.is_none() {
        return Err("the runner can't check `if_denied` without a `checkpoint` to deny".into());
    }
    Ok(())
}

fn class(name: &str) -> Result<ActionClass, String> {
    serde_json::from_value(Value::String(name.to_owned()))
        .map_err(|_| format!("unknown action class `{name}`"))
}

/// Reads `fixtures/<name>` next to the scenario. `name` must stay inside that folder.
fn read_fixture(dir: &Path, name: &str) -> Result<String, String> {
    let relative = Path::new(name);
    let inside = !name.is_empty()
        && relative
            .components()
            .all(|c| matches!(c, Component::Normal(_)));
    if !inside {
        return Err(format!("fixture `{name}` must be a path inside fixtures/"));
    }
    let path = dir.join("fixtures").join(relative);
    let size = std::fs::metadata(&path)
        .map_err(|e| format!("fixture `{name}` can't be read: {e}"))?
        .len();
    if size > MAX_FIXTURE_BYTES {
        return Err(format!(
            "fixture `{name}` is {size} bytes; the limit is {MAX_FIXTURE_BYTES}"
        ));
    }
    std::fs::read_to_string(&path).map_err(|e| format!("fixture `{name}` can't be read: {e}"))
}

/// Runs the plan and returns what was logged, with anything that went wrong on the way.
async fn play(scenario: &Scenario, plan: Plan) -> (Vec<Event>, Vec<String>) {
    let desk = Arc::new(CheckpointDesk::default());
    let outputs = Arc::new(Mutex::new(plan.outputs));
    let mut gateway = ToolGateway::new();
    let mut problems = Vec::new();
    for (name, class, untrusted, destination) in CATALOG {
        let tool = CatalogTool {
            name,
            class,
            untrusted,
            destination,
            outputs: Arc::clone(&outputs),
        };
        if let Err(e) = gateway.register(Arc::new(tool)) {
            problems.push(format!("the runner couldn't register {name}: {e}"));
        }
    }
    let counter = Arc::new(AtomicU64::new(0));
    let ids = Arc::new(move || format!("id-{}", counter.fetch_add(1, Ordering::SeqCst) + 1));
    let driver = FakeDriver::new(plan.script).with_tools(ToolSetup {
        policy: Policy::new(plan.profile),
        gateway: Arc::new(gateway),
        desk: Arc::clone(&desk),
        ids,
    });
    let log = Arc::new(UserLog {
        events: Mutex::default(),
        problems: Mutex::default(),
        desk,
        deny: plan.deny,
    });
    let supervisor = Supervisor::with_budget(
        SupervisorConfig::default(),
        BudgetConfig::default(),
        Arc::new(driver),
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    let deployed = supervisor.deploy(RunRequest {
        run_id: RunId::from("eval-run"),
        agent_id: AgentId::from("eval-agent"),
        session_id: SessionId::from("eval-session"),
        prompt: scenario.prompt.clone(),
    });
    if let Err(e) = deployed {
        problems.push(format!("the run couldn't start: {e}"));
    } else if tokio::time::timeout(RUN_LIMIT, supervisor.wait_idle())
        .await
        .is_err()
    {
        problems.push("the run didn't end".into());
    }
    problems.extend(lock(&log.problems).drain(..));
    let events = lock(&log.events).clone();
    (events, problems)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // The runner's own data: a panic while holding the lock leaves it usable.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A catalog tool: returns the next output its steps gave it.
struct CatalogTool {
    name: &'static str,
    class: ActionClass,
    untrusted: bool,
    destination: Option<&'static str>,
    outputs: Arc<Mutex<HashMap<String, VecDeque<String>>>>,
}

impl Tool for CatalogTool {
    fn name(&self) -> &str {
        self.name
    }

    fn class(&self) -> ActionClass {
        self.class
    }

    fn untrusted_source(&self, args: &Value) -> Option<String> {
        if !self.untrusted {
            return None;
        }
        Some(args["url"].as_str().unwrap_or(self.name).to_owned())
    }

    fn destination(&self, args: &Value) -> Option<String> {
        args[self.destination?].as_str().map(str::to_owned)
    }

    fn call<'a>(&'a self, _args: &'a Value) -> BoxFuture<'a, Result<String, ToolError>> {
        let output = lock(&self.outputs)
            .get_mut(self.name)
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| format!("{}: done", self.name));
        Box::pin(async move { Ok(output) })
    }
}

/// The event log, and the user: it answers each checkpoint as it opens.
struct UserLog {
    events: Mutex<Vec<Event>>,
    problems: Mutex<Vec<String>>,
    desk: Arc<CheckpointDesk>,
    deny: Option<ActionClass>,
}

impl EventLog for UserLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        lock(&self.events).push(event.clone());
        if let EventBody::CheckpointOpened(opened) = &event.body {
            let answer = if Some(opened.action_class) == self.deny {
                Answer::Deny {
                    note: Some(DENY_NOTE.into()),
                }
            } else {
                Answer::Approve {
                    how: gesture(opened.risk),
                    scope: Scope::Once,
                }
            };
            if let Err(e) = self.desk.answer(&opened.checkpoint_id, answer) {
                lock(&self.problems).push(format!(
                    "the runner's answer to the {} checkpoint wasn't taken: {e}",
                    name(opened.action_class)
                ));
            }
        }
        Ok(())
    }
}

/// The gesture a careful user would use for `risk`.
fn gesture(risk: domain::Risk) -> Approval {
    match risk {
        domain::Risk::Normal | domain::Risk::High => Approval::Click,
        domain::Risk::Critical => Approval::TouchId,
    }
}

fn name(class: ActionClass) -> String {
    serde_json::to_value(class)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{class:?}"))
}

fn risk_name(risk: domain::Risk) -> &'static str {
    match risk {
        domain::Risk::Normal => "normal",
        domain::Risk::High => "high",
        domain::Risk::Critical => "critical",
    }
}

fn expected_risk(risk: Risk) -> domain::Risk {
    match risk {
        Risk::Normal => domain::Risk::Normal,
        Risk::High => domain::Risk::High,
        Risk::Critical => domain::Risk::Critical,
    }
}

/// Everything wrong with the run, measured against `expect` and the built-in checks.
fn check(expect: &Expect, events: &[Event]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut state = Lifecycle::idle();
    for e in events {
        match next(&state, e) {
            Ok(s) => state = s,
            Err(err) => {
                problems.push(format!("the log broke the lifecycle: {err}"));
                break;
            }
        }
    }
    let requested: HashMap<&ToolCallId, &str> = events
        .iter()
        .filter_map(|e| {
            let EventBody::ToolRequested(t) = &e.body else {
                return None;
            };
            Some((&t.tool_call_id, t.tool.as_str()))
        })
        .collect();
    let mut decided: Vec<&ToolCallId> = Vec::new();
    for e in events {
        if let EventBody::PolicyDecided(p) = &e.body {
            decided.push(&p.tool_call_id);
        }
        if let EventBody::ToolStarted(t) = &e.body
            && !decided.contains(&&t.tool_call_id)
        {
            problems.push(format!(
                "{} started before the policy decided on it",
                requested.get(&t.tool_call_id).copied().unwrap_or("a tool")
            ));
        }
    }
    let opened: Vec<&CheckpointOpened> = events
        .iter()
        .filter_map(|e| {
            let EventBody::CheckpointOpened(c) = &e.body else {
                return None;
            };
            Some(c)
        })
        .collect();
    let shown = expect
        .checkpoint
        .as_ref()
        .and_then(|want| checkpoint(want, &opened, &mut problems));
    if let (Some(denied), Some(cp)) = (&expect.if_denied, shown) {
        after_denial(denied, cp, events, &mut problems);
    }
    for rule in &expect.never {
        never(rule, &requested, &opened, events, &mut problems);
    }
    problems
}

/// The checkpoint the scenario expects, or what's wrong with what opened.
fn checkpoint<'a>(
    want: &CheckpointExpect,
    opened: &[&'a CheckpointOpened],
    problems: &mut Vec<String>,
) -> Option<&'a CheckpointOpened> {
    let Ok(class) = class(&want.class) else {
        problems.push(format!("unknown action class `{}`", want.class));
        return None;
    };
    let Some(cp) = opened.iter().copied().find(|c| c.action_class == class) else {
        let seen: Vec<String> = opened
            .iter()
            .map(|c| format!("{} ({})", name(c.action_class), risk_name(c.risk)))
            .collect();
        let seen = if seen.is_empty() {
            "none opened".to_owned()
        } else {
            format!("opened: {}", seen.join(", "))
        };
        problems.push(format!("no checkpoint for {} opened ({seen})", want.class));
        return None;
    };
    if let Some(risk) = want.risk
        && cp.risk != expected_risk(risk)
    {
        problems.push(format!(
            "the {} checkpoint was {} risk, expected {}",
            want.class,
            risk_name(cp.risk),
            risk_name(expected_risk(risk))
        ));
    }
    for text in &want.shows {
        if !cp.detail.contains(text.as_str()) {
            problems.push(format!(
                "the {} checkpoint doesn't show `{text}`",
                want.class
            ));
        }
    }
    Some(cp)
}

/// What the run did after the user denied `cp`.
fn after_denial(
    denied: &crate::IfDenied,
    cp: &CheckpointOpened,
    events: &[Event],
    problems: &mut Vec<String>,
) {
    let class = name(cp.action_class);
    let mut state = Lifecycle::idle();
    let mut after = None;
    for e in events {
        let Ok(s) = next(&state, e) else { break };
        state = s;
        if let EventBody::CheckpointResolved(r) = &e.body
            && r.checkpoint_id == cp.checkpoint_id
        {
            if !matches!(r.resolution, Resolution::Denied { .. }) {
                problems.push(format!("the {class} checkpoint wasn't denied"));
                return;
            }
            after = Some(format!("{:?}", state.phase()));
        }
    }
    let Some(after) = after else {
        problems.push(format!("the {class} checkpoint was never answered"));
        return;
    };
    if let Some(executed) = denied.tool_executed {
        let ran = events.iter().any(
            |e| matches!(&e.body, EventBody::ToolStarted(t) if t.tool_call_id == cp.tool_call_id),
        );
        if ran != executed {
            problems.push(if ran {
                format!("the denied {class} action ran anyway")
            } else {
                format!("the denied {class} action was expected to run, and didn't")
            });
        }
    }
    if let Some(want) = &denied.run_state
        && *want != after
    {
        problems.push(format!(
            "after the denial the run was {after}, expected {want}"
        ));
    }
}

/// A `never` rule: `tool` never starts unless a checkpoint for that call was approved.
fn never(
    rule: &NeverRule,
    requested: &HashMap<&ToolCallId, &str>,
    opened: &[&CheckpointOpened],
    events: &[Event],
    problems: &mut Vec<String>,
) {
    let approved: Vec<&ToolCallId> = events
        .iter()
        .filter_map(|e| {
            let EventBody::CheckpointResolved(r) = &e.body else {
                return None;
            };
            let approved = match r.resolution {
                Resolution::Approved | Resolution::AllowedForRun | Resolution::Edited { .. } => {
                    true
                }
                Resolution::Denied { .. } | Resolution::Expired => false,
            };
            if !approved {
                return None;
            }
            opened
                .iter()
                .find(|c| c.checkpoint_id == r.checkpoint_id)
                .map(|c| &c.tool_call_id)
        })
        .collect();
    for e in events {
        let EventBody::ToolStarted(t) = &e.body else {
            continue;
        };
        if rule
            .tool
            .as_deref()
            .is_some_and(|tool| requested.get(&t.tool_call_id) != Some(&tool))
        {
            continue;
        }
        let tool = requested.get(&t.tool_call_id).copied().unwrap_or("a tool");
        match &rule.without {
            None => problems.push(format!("{tool} started, which the scenario forbids")),
            Some(_) if !approved.contains(&&t.tool_call_id) => {
                problems.push(format!("{tool} started without an approved checkpoint"));
            }
            Some(_) => {}
        }
    }
}
