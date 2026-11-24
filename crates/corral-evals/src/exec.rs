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
//!   (TP-ADP-02), and each run's events follow the lifecycle.
//! - **Several runs at once** (EV-COST-01 and 03, TP-EVAL-14): `runs` copies start together,
//!   spread over `agents` round robin, through one supervisor and one spend ledger, at the
//!   scenario's price and caps. `expect.spend` grades the bill from the log: each run's, each
//!   agent's and all agents' (the reported cost, or tokens × price if that's more), and how many
//!   runs a cap refused. A `usage` step without a cost is billed the way an honest provider bills:
//!   its output stops at the request's max output tokens (which Corral sets from the room left
//!   under the caps, security model T25) and is priced. A given cost is reported as is, like a
//!   provider that overbills, which Corral can only charge afterwards.
//!
//! Time is tokio's paused clock, so a checkpoint timeout of 30 minutes passes instantly.

use crate::{
    CapScope, CheckpointExpect, Expect, NeverRule, RecallExpect, Risk, Scenario, ScenarioResult,
    SpendExpect, Status,
};
use corral_adapters::fake::{FakeDriver, FakeScript, ToolSetup};
use corral_core::budget::{BudgetConfig, LedgerConfig, LocalMonths, Price, SpendLedger, usd};
use corral_core::clock::ManualClock;
use corral_core::domain::{self, ActionClass};
use corral_core::event::{
    CancelledBy, CheckpointOpened, Event, EventBody, FailureReason, Resolution,
};
use corral_core::ids::{AgentId, RunId, SessionId, ToolCallId};
use corral_core::lifecycle::{Lifecycle, next};
use corral_core::supervisor::{
    EventLog, LogError, RecallReport, RunRequest, Supervisor, SupervisorConfig,
};
use corral_policy::{Approval, Policy, Profile, Scope};
use corral_tools::{Answer, BoxFuture, CheckpointDesk, Tool, ToolError, ToolGateway};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::time::Instant;

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
            let caps = plan.caps;
            let (events, recall, mut problems) = play(scenario, plan).await;
            problems.extend(check(&scenario.expect, &caps, &events));
            if let (Some(want), Some(seen)) = (&scenario.expect.recall, &recall) {
                recall_check(want, seen, scenario.runs, &events, &mut problems);
            }
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
    caps: Caps,
}

/// The fake model's price and the caps the runs are held to.
#[derive(Debug, Clone, Copy)]
struct Caps {
    price: Price,
    run: u64,
    agent_month: u64,
    all_month: u64,
}

impl Caps {
    fn of(scenario: &Scenario) -> Self {
        let budget = scenario.budget.unwrap_or_default();
        let ledger = LedgerConfig::default();
        Self {
            price: budget
                .price_per_million_tokens
                .map_or(Price::per_million_tokens(0, 0), |p| {
                    Price::per_million_tokens(p.input_micro_usd, p.output_micro_usd)
                }),
            run: budget
                .run_cap_micro_usd
                .unwrap_or(BudgetConfig::default().per_run_cap_micro_usd),
            agent_month: budget
                .agent_month_cap_micro_usd
                .unwrap_or(ledger.agent_month_cap_micro_usd),
            all_month: budget
                .global_month_cap_micro_usd
                .unwrap_or(ledger.global_month_cap_micro_usd),
        }
    }

    /// What a turn is billed: the reported cost, or tokens × price if that's more.
    fn billed(&self, input_tokens: u64, output_tokens: u64, reported: u64) -> u64 {
        let floor = self
            .price
            .input_cost(input_tokens)
            .saturating_add(self.price.output_cost(output_tokens));
        reported.max(floor)
    }
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
    let caps = Caps::of(scenario);
    let mut script = FakeScript::default();
    let mut outputs: HashMap<String, VecDeque<String>> = HashMap::new();
    for (i, step) in scenario.steps.iter().enumerate() {
        let n = i + 1;
        if let Some(usage) = step.usage {
            // Without a cost, the fake bills like an honest provider: output stops at the
            // request's limit and is priced. A given cost is reported as is, like a provider
            // that overbills.
            script = match usage.cost_micro_usd {
                Some(cost) => script.usage(usage.input_tokens, usage.output_tokens, cost),
                None => script.billed(usage.input_tokens, usage.output_tokens),
            };
            continue;
        }
        if step.new_turn.is_some() {
            script = script.request();
            continue;
        }
        if let Some(ms) = step.wait_ms {
            script = script.wait(Duration::from_millis(ms));
            continue;
        }
        if let Some(ms) = step.stubborn_ms {
            script = script.stubborn(Duration::from_millis(ms));
            continue;
        }
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
        caps,
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

/// Stop all, as the runner pressed it: when, how much was logged by then, and what it reported.
struct Recall {
    pressed: Instant,
    logged: usize,
    report: RecallReport,
    /// When each run ended, on the runner's clock.
    ended: HashMap<RunId, Instant>,
}

/// Runs the plan and returns what was logged, how Stop all went if the scenario pressed it, and
/// anything that went wrong on the way.
async fn play(scenario: &Scenario, plan: Plan) -> (Vec<Event>, Option<Recall>, Vec<String>) {
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
    let driver = FakeDriver::new(plan.script)
        .priced(plan.caps.price)
        .with_tools(ToolSetup {
            policy: Policy::new(plan.profile),
            gateway: Arc::new(gateway),
            desk: Arc::clone(&desk),
            ids,
        });
    let log = Arc::new(UserLog {
        events: Mutex::default(),
        ended: Mutex::default(),
        problems: Mutex::default(),
        desk,
        deny: plan.deny,
    });
    let Some(months) = LocalMonths::fixed(0) else {
        problems.push("the runner couldn't count months in UTC".into());
        return (Vec::new(), None, problems);
    };
    let ledger = SpendLedger::new(
        LedgerConfig {
            agent_month_cap_micro_usd: plan.caps.agent_month,
            global_month_cap_micro_usd: plan.caps.all_month,
        },
        months,
    );
    let supervisor = Supervisor::with_ledger(
        SupervisorConfig::default(),
        BudgetConfig {
            per_run_cap_micro_usd: plan.caps.run,
        },
        Arc::new(ledger),
        Arc::new(driver),
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    for i in 1..=scenario.runs {
        let agent = (i - 1) % scenario.agents + 1;
        let deployed = supervisor.deploy(RunRequest {
            run_id: RunId::from(format!("eval-run-{i}").as_str()),
            agent_id: AgentId::from(format!("eval-agent-{agent}").as_str()),
            session_id: SessionId::from(format!("eval-session-{i}").as_str()),
            prompt: scenario.prompt.clone(),
        });
        if let Err(e) = deployed {
            problems.push(format!("run {i} couldn't start: {e}"));
        }
    }
    let pressed = match scenario.recall_all_after_ms {
        Some(ms) => {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            let at = Instant::now();
            let logged = lock(&log.events).len();
            let report = supervisor.recall_all().await;
            Some((at, logged, report))
        }
        None => None,
    };
    // With no run started this returns at once, and the problems above say why.
    if tokio::time::timeout(RUN_LIMIT, supervisor.wait_idle())
        .await
        .is_err()
    {
        problems.push("the run didn't end".into());
    }
    problems.extend(lock(&log.problems).drain(..));
    let events = lock(&log.events).clone();
    let recall = pressed.map(|(pressed, logged, report)| Recall {
        pressed,
        logged,
        report,
        ended: lock(&log.ended).clone(),
    });
    (events, recall, problems)
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
    /// When each run ended, on the runner's clock (the log's own times are a fixed clock).
    ended: Mutex<HashMap<RunId, Instant>>,
    problems: Mutex<Vec<String>>,
    desk: Arc<CheckpointDesk>,
    deny: Option<ActionClass>,
}

impl EventLog for UserLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        lock(&self.events).push(event.clone());
        if matches!(
            event.body,
            EventBody::RunCompleted(_) | EventBody::RunFailed(_) | EventBody::RunCancelled(_)
        ) {
            lock(&self.ended).insert(event.run_id.clone(), Instant::now());
        }
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

/// Everything wrong with the runs, measured against `expect` and the built-in checks.
fn check(expect: &Expect, caps: &Caps, events: &[Event]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut states: HashMap<&RunId, Lifecycle> = HashMap::new();
    let mut broken: Vec<&RunId> = Vec::new();
    for e in events {
        if broken.contains(&&e.run_id) {
            continue;
        }
        let state = states.entry(&e.run_id).or_insert_with(Lifecycle::idle);
        match next(state, e) {
            Ok(s) => *state = s,
            Err(err) => {
                problems.push(format!("{}'s log broke the lifecycle: {err}", e.run_id));
                broken.push(&e.run_id);
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
    if let Some(want) = &expect.spend {
        spend(want, caps, events, &mut problems);
    }
    problems
}

/// How Stop all went (EV-STOP-*): every run stopped by it, nothing started after the press, the
/// runs ended within the limits, and the drivers dropped for ignoring Stop counted.
fn recall_check(
    want: &RecallExpect,
    seen: &Recall,
    runs: u32,
    events: &[Event],
    problems: &mut Vec<String>,
) {
    let mut halts: Vec<u64> = Vec::new();
    for i in 1..=runs {
        let run = RunId::from(format!("eval-run-{i}").as_str());
        let stopped = events.iter().any(|e| {
            e.run_id == run
                && matches!(&e.body, EventBody::RunCancelled(c) if c.by == CancelledBy::RecallAll)
        });
        if !stopped {
            problems.push(format!(
                "{run} had already ended when Stop all was pressed, so it tested nothing"
            ));
            continue;
        }
        match seen.ended.get(&run) {
            Some(at) => halts.push(ms(at.saturating_duration_since(seen.pressed))),
            None => problems.push(format!("{run} never logged its end")),
        }
    }
    for e in events.iter().skip(seen.logged) {
        if let EventBody::ToolStarted(t) = &e.body {
            problems.push(format!(
                "{} started a tool ({}) after Stop all",
                e.run_id, t.tool_call_id
            ));
        }
    }
    halts.sort_unstable();
    if let (Some(limit), Some(p95)) = (want.p95_within_ms, percentile_95(&halts))
        && p95 > limit
    {
        problems.push(format!(
            "95% of the runs took up to {p95} ms to end after Stop all; the limit is {limit} ms"
        ));
    }
    if let (Some(limit), Some(&slowest)) = (want.all_within_ms, halts.last())
        && slowest > limit
    {
        problems.push(format!(
            "the slowest run took {slowest} ms to end after Stop all; the limit is {limit} ms"
        ));
    }
    let forced = seen.report.forced;
    if let Some(least) = want.forced_at_least
        && forced < least as usize
    {
        problems.push(format!(
            "{forced} runs ignored Stop and were dropped; at least {least} should have"
        ));
    }
    if let Some(most) = want.forced_at_most
        && forced > most as usize
    {
        problems.push(format!(
            "{forced} runs ignored Stop and were dropped; at most {most} may"
        ));
    }
}

/// The value 95% of `sorted` are at or under (nearest rank).
fn percentile_95(sorted: &[u64]) -> Option<u64> {
    let rank = (sorted.len() * 95).div_ceil(100);
    sorted.get(rank.checked_sub(1)?).copied()
}

fn ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// The bill, graded from the log against the caps `want` names, and the runs a cap refused.
fn spend(want: &SpendExpect, caps: &Caps, events: &[Event], problems: &mut Vec<String>) {
    let mut agent_of: HashMap<&RunId, &AgentId> = HashMap::new();
    let mut by_run: BTreeMap<&RunId, u64> = BTreeMap::new();
    let mut by_agent: BTreeMap<&AgentId, u64> = BTreeMap::new();
    let mut all: u64 = 0;
    let mut refused: u32 = 0;
    for e in events {
        match &e.body {
            EventBody::RunQueued(queued) => {
                agent_of.insert(&e.run_id, &queued.agent_id);
            }
            EventBody::UsageReported(usage) => {
                let billed = caps.billed(
                    usage.input_tokens,
                    usage.output_tokens,
                    usage.cost_micro_usd,
                );
                let run = by_run.entry(&e.run_id).or_insert(0);
                *run = run.saturating_add(billed);
                all = all.saturating_add(billed);
                match agent_of.get(&e.run_id) {
                    Some(agent) => {
                        let spent = by_agent.entry(agent).or_insert(0);
                        *spent = spent.saturating_add(billed);
                    }
                    None => {
                        problems.push(format!("{} reported usage before it was queued", e.run_id))
                    }
                }
            }
            EventBody::RunFailed(failed) if failed.reason == FailureReason::BudgetHardCap => {
                refused += 1;
            }
            EventBody::RunStarted(_)
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
            | EventBody::BudgetWarned(_)
            | EventBody::RunPaused(_)
            | EventBody::RunResumed(_)
            | EventBody::RunCompleted(_)
            | EventBody::RunFailed(_)
            | EventBody::RunCancelled(_)
            | EventBody::SessionClosed(_) => {}
        }
    }
    for scope in &want.never_over {
        match scope {
            CapScope::Run => {
                for (run, billed) in &by_run {
                    if *billed > caps.run {
                        problems.push(format!(
                            "{run} was billed {}; its cap is {}",
                            usd(*billed),
                            usd(caps.run)
                        ));
                    }
                }
            }
            CapScope::AgentMonth => {
                for (agent, billed) in &by_agent {
                    if *billed > caps.agent_month {
                        problems.push(format!(
                            "{agent} was billed {} this month; its cap is {}",
                            usd(*billed),
                            usd(caps.agent_month)
                        ));
                    }
                }
            }
            CapScope::AllMonth => {
                if all > caps.all_month {
                    problems.push(format!(
                        "all agents were billed {} this month; the cap for all of them is {}",
                        usd(all),
                        usd(caps.all_month)
                    ));
                }
            }
        }
    }
    if let Some(least) = want.refused_at_least
        && refused < least
    {
        let runs = if least == 1 { "run" } else { "runs" };
        problems.push(format!(
            "expected a cap to refuse at least {least} {runs}; {refused} were refused"
        ));
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use corral_core::event::{RunFailed, RunQueued, UsageReported};
    use corral_core::ids::SpanId;

    fn ev(run: &str, body: EventBody) -> Event {
        Event {
            run_id: RunId::from(run),
            seq: 1,
            ts: 1_000,
            span_id: SpanId::from("span"),
            parent_span_id: None,
            body,
        }
    }

    fn queued(run: &str, agent: &str) -> Event {
        ev(
            run,
            EventBody::RunQueued(RunQueued {
                agent_id: AgentId::from(agent),
                session_id: SessionId::from("s"),
                prompt: "p".into(),
            }),
        )
    }

    fn billed(run: &str, cost: u64) -> Event {
        ev(
            run,
            EventBody::UsageReported(UsageReported {
                input_tokens: 0,
                output_tokens: 0,
                cost_micro_usd: cost,
            }),
        )
    }

    fn failed(run: &str, reason: FailureReason) -> Event {
        ev(
            run,
            EventBody::RunFailed(RunFailed {
                reason,
                message: "m".into(),
            }),
        )
    }

    fn caps(run: u64) -> Caps {
        Caps {
            price: Price::per_million_tokens(0, 0),
            run,
            agent_month: u64::MAX,
            all_month: u64::MAX,
        }
    }

    fn graded(want: &SpendExpect, caps: &Caps, events: &[Event]) -> Vec<String> {
        let mut problems = Vec::new();
        spend(want, caps, events, &mut problems);
        problems
    }

    #[test]
    fn tp_eval_14_a_run_billed_exactly_its_cap_passes_and_one_past_it_fails() {
        let want = SpendExpect {
            never_over: vec![CapScope::Run],
            refused_at_least: None,
        };
        let at = [queued("r1", "a"), billed("r1", 600), billed("r1", 400)];
        assert_eq!(graded(&want, &caps(1_000), &at), Vec::<String>::new());
        let over = [queued("r1", "a"), billed("r1", 1_001)];
        assert_eq!(
            graded(&want, &caps(1_000), &over),
            ["r1 was billed $0.0011; its cap is $0.0010"]
        );
        let under = [queued("r1", "a"), billed("r1", 999)];
        assert_eq!(graded(&want, &caps(1_000), &under), Vec::<String>::new());
    }

    #[test]
    fn tp_eval_14_only_runs_a_cap_stopped_count_as_refused() {
        let want = SpendExpect {
            never_over: Vec::new(),
            refused_at_least: Some(1),
        };
        let other = [queued("r1", "a"), failed("r1", FailureReason::Adapter)];
        assert_eq!(
            graded(&want, &caps(1_000), &other),
            ["expected a cap to refuse at least 1 run; 0 were refused"]
        );
        let capped = [
            queued("r1", "a"),
            failed("r1", FailureReason::BudgetHardCap),
        ];
        assert_eq!(graded(&want, &caps(1_000), &capped), Vec::<String>::new());
        let two = SpendExpect {
            never_over: Vec::new(),
            refused_at_least: Some(2),
        };
        assert_eq!(
            graded(&two, &caps(1_000), &capped),
            ["expected a cap to refuse at least 2 runs; 1 were refused"]
        );
    }

    #[test]
    fn tp_eval_14_usage_from_a_run_never_queued_is_a_problem() {
        let want = SpendExpect {
            never_over: vec![CapScope::AgentMonth],
            refused_at_least: None,
        };
        assert_eq!(
            graded(&want, &caps(1_000), &[billed("r9", 1)]),
            ["r9 reported usage before it was queued"]
        );
    }

    // ---- TP-EVAL-18: the pause comparison, on hand-built logs -------------------------------

    fn said(run: &str, text: &str) -> Event {
        ev(
            run,
            EventBody::ModelDelta(corral_core::event::ModelDelta { text: text.into() }),
        )
    }

    fn pause_events(run: &str) -> [Event; 2] {
        [
            ev(
                run,
                EventBody::RunPaused(corral_core::event::RunPaused {
                    reason: corral_core::event::PauseReason::User,
                }),
            ),
            ev(
                run,
                EventBody::RunResumed(corral_core::event::RunResumed {}),
            ),
        ]
    }

    fn done(run: &str) -> Event {
        ev(
            run,
            EventBody::RunCompleted(corral_core::event::RunCompleted { summary: None }),
        )
    }

    fn compared(paused: &[Event], unpaused: &[Event]) -> Vec<String> {
        let mut problems = Vec::new();
        pause_check(1, paused, unpaused, &mut problems);
        problems
    }

    #[test]
    fn tp_eval_18_a_difference_names_the_first_event_that_differs() {
        let [p, r] = pause_events("eval-run-1");
        let paused = [
            queued("eval-run-1", "a"),
            said("eval-run-1", "one"),
            p,
            r,
            said("eval-run-1", "two"),
            done("eval-run-1"),
        ];
        let unpaused = [
            queued("eval-run-1", "a"),
            said("eval-run-1", "one"),
            done("eval-run-1"),
        ];
        assert_eq!(
            compared(&paused, &unpaused),
            [
                "eval-run-1 logged something different from the same run unpaused: event 3 was \
              model.delta paused and run.completed unpaused"
            ]
        );
    }

    #[test]
    fn tp_eval_18_only_the_pause_and_resume_are_left_out() {
        let [p, r] = pause_events("eval-run-1");
        let paused = [
            queued("eval-run-1", "a"),
            p,
            r,
            said("eval-run-1", "one"),
            queued("eval-run-2", "b"),
            done("eval-run-1"),
        ];
        let unpaused = [
            queued("eval-run-1", "a"),
            said("eval-run-1", "one"),
            done("eval-run-1"),
        ];
        assert!(
            compared(&paused, &unpaused).is_empty(),
            "other runs don't count"
        );
        let [p, _] = pause_events("eval-run-1");
        let never_resumed = [queued("eval-run-1", "a"), p, done("eval-run-1")];
        assert_eq!(
            compared(
                &never_resumed,
                &[queued("eval-run-1", "a"), done("eval-run-1")]
            ),
            ["eval-run-1 paused 1 times but resumed 0"]
        );
    }
}
