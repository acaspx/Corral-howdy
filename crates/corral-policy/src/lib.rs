//! The pure, deny-by-default policy engine that decides Allow / Ask / Deny for every agent action
//! (`docs/01-system-design.md` §5, ADR-003). Pure and deterministic: the same policy, action and
//! run give the same decision, with no I/O and no clock.
//!
//! - **Rules per action class.** Every class has a default ([`default_rule`]). An agent's
//!   [`Profile`] can widen or narrow any class, but never past the global [`Ceilings`]: money,
//!   deleting and running commands always ask at least, and `unknown` is always denied (T3).
//! - **Risk** comes from the class ([`risk_of`]) and sets the gesture that can approve
//!   ([`can_approve`]): voice can't approve high or critical, and critical needs Touch ID (T5).
//! - **Allow for the rest of this run** lives in [`RunGrants`], never in the profile, so it ends
//!   with the run (TP-POL-04). For high and critical classes a grant is pinned to the destination
//!   it was given for (T21).
//! - **Approve once per exact call** (TP-POL-08, T4): once the user approves a normal-risk
//!   call, the exact same call (same tool, same arguments in any key order) doesn't ask again in
//!   the run. High and critical calls, spend limits, edited calls and ways out on a tainted run
//!   still ask, and so does every call from the loop detector's limit on.
//! - **Taint** (T28): once a run has read untrusted content, grants on the classes that can send
//!   data out (`net.write`, `comms.send`, `net.read.unlisted`) stop applying, and the checkpoint
//!   names the untrusted source. Taint never clears within a run.
//! - **The gate** (T2): one [`Gate`] per run. [`Gate::check`] is the only way to an
//!   [`AllowToken`], and the tool gateway (`corral-tools`) runs nothing without one. A token can't
//!   be made, copied or reused outside this crate:
//!
//! ```compile_fail
//! // No constructor: a token comes only from Gate::check or an approved checkpoint.
//! let forged = corral_policy::AllowToken::default();
//! ```
//!
//! ```compile_fail
//! fn reuse(token: corral_policy::AllowToken) -> (corral_policy::AllowToken, corral_policy::AllowToken) {
//!     (token.clone(), token)
//! }
//! ```

use corral_core::domain::{ActionClass, Risk};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What a profile or ceiling says about a class, from strictest to most permissive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Rule {
    /// Never.
    Deny,
    /// Stop at a checkpoint.
    Ask,
    /// Go ahead.
    Allow,
}

/// What happens when a checkpoint isn't answered in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OnTimeout {
    /// The action is denied and the agent is told.
    Deny,
    /// The run is cancelled.
    CancelRun,
}

/// The decision for one action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Go ahead.
    Allow,
    /// Stop at a checkpoint.
    Ask {
        /// How much is at stake; sets the gesture that can approve.
        risk: Risk,
        /// Why, in plain words, for the checkpoint.
        reason: String,
    },
    /// Never; the agent is told why.
    Deny {
        /// Why, in plain words.
        reason: String,
    },
}

/// One action an agent wants to take, already mapped to its class by the adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    /// Its class.
    pub class: ActionClass,
    /// The tool, e.g. `email.send`.
    pub tool: String,
    /// Where it goes: a recipient, a domain or a path root. Grants on high and critical classes
    /// are pinned to it.
    pub destination: Option<String>,
    /// For deletes: the file is outside the agent's sandbox roots (critical).
    pub outside_sandbox: bool,
}

impl Action {
    /// An action of `class` through `tool`.
    pub fn new(class: ActionClass, tool: impl Into<String>) -> Self {
        Self {
            class,
            tool: tool.into(),
            destination: None,
            outside_sandbox: false,
        }
    }

    /// Sets where it goes.
    pub fn to(mut self, destination: impl Into<String>) -> Self {
        self.destination = Some(destination.into());
        self
    }

    /// Marks a delete as outside the sandbox roots.
    pub fn outside_sandbox(mut self) -> Self {
        self.outside_sandbox = true;
        self
    }
}

/// Why a profile was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    /// Not valid TOML, an unknown key, an unknown class or a bad value.
    #[error("the profile couldn't be read: {0}")]
    Parse(String),
    /// Two rules for one class.
    #[error("the profile has two rules for {0:?}")]
    DuplicateRule(ActionClass),
    /// A rule that tries to allow or ask for `unknown`.
    #[error("`unknown` actions can only be denied")]
    UnknownCanOnlyBeDenied,
    /// A checkpoint timeout that isn't like `30m`, `90s` or `2h`, or is zero or over a week.
    #[error("checkpoint_timeout must be like 30m, 90s or 2h, from 1s to 168h: {0}")]
    BadDuration(String),
    /// A loop limit under 2 or over 1,000.
    #[error("loop_limit must be from 2 to 1,000 (got {0})")]
    BadLoopLimit(u32),
    /// An edit that would allow more than the global ceiling (security model T7).
    #[error("{} can be at most {ceiling:?} for any agent", what(*class))]
    AboveCeiling {
        /// The class.
        class: ActionClass,
        /// Its ceiling.
        ceiling: Rule,
    },
    /// A class the policy editor doesn't change: `unknown` is always denied, and provider-side
    /// tools are switched in the agent editor.
    #[error("{} can't be changed in the policy editor", what(*.0))]
    NotEditable(ActionClass),
    /// An allowed domain that isn't a plain hostname or a `*.` wildcard over one.
    #[error(
        "{0:?} isn't a domain like example.com or *.example.com (use punycode for international names)"
    )]
    BadDomain(String),
    /// More allowed domains than [`MAX_NET_ALLOW`].
    #[error("at most {MAX_NET_ALLOW} allowed domains (got {0})")]
    TooManyDomains(usize),
    /// The profile couldn't be written as TOML.
    #[error("the profile couldn't be written: {0}")]
    Write(String),
}

/// How many identical calls a run makes before each further one asks (EV-COST-02).
pub const DEFAULT_LOOP_LIMIT: u32 = 5;
/// The range a profile's `loop_limit` may take.
pub const LOOP_LIMITS: std::ops::RangeInclusive<u32> = 2..=1_000;

/// One agent's policy profile (`[profiles.<name>]` in a manifest).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// Folders the agent's file tools may use.
    pub sandbox_roots: Vec<String>,
    /// Domains `net.read` may reach without asking (globs like `*.wikipedia.org`).
    pub net_allow: Vec<String>,
    rules: HashMap<ActionClass, Rule>,
    /// How long a checkpoint waits.
    pub checkpoint_timeout: Duration,
    /// What happens when it isn't answered.
    pub on_timeout: OnTimeout,
    /// From this many identical calls in a run on, each asks before it runs (T9).
    pub loop_limit: u32,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            sandbox_roots: Vec::new(),
            net_allow: Vec::new(),
            rules: HashMap::new(),
            checkpoint_timeout: Duration::from_secs(30 * 60),
            on_timeout: OnTimeout::Deny,
            loop_limit: DEFAULT_LOOP_LIMIT,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileFile {
    #[serde(default)]
    sandbox_roots: Vec<String>,
    #[serde(default)]
    net_allow: Vec<String>,
    #[serde(default)]
    rules: Vec<RuleFile>,
    #[serde(default)]
    checkpoint_timeout: Option<String>,
    #[serde(default)]
    on_timeout: Option<OnTimeout>,
    #[serde(default)]
    loop_limit: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleFile {
    class: ActionClass,
    decision: Rule,
}

impl Profile {
    /// Reads a profile table. Strict: unknown keys, unknown classes, duplicate rules and a rule
    /// that would open `unknown` are errors. Missing keys take the defaults.
    pub fn from_toml(text: &str) -> Result<Self, PolicyError> {
        let file: ProfileFile =
            toml::from_str(text).map_err(|e| PolicyError::Parse(e.message().to_owned()))?;
        let mut rules = HashMap::new();
        for rule in file.rules {
            if rule.class == ActionClass::Unknown && rule.decision != Rule::Deny {
                return Err(PolicyError::UnknownCanOnlyBeDenied);
            }
            if rules.insert(rule.class, rule.decision).is_some() {
                return Err(PolicyError::DuplicateRule(rule.class));
            }
        }
        let defaults = Self::default();
        Ok(Self {
            sandbox_roots: file.sandbox_roots,
            net_allow: file.net_allow,
            rules,
            checkpoint_timeout: match file.checkpoint_timeout {
                Some(text) => parse_duration(&text)?,
                None => defaults.checkpoint_timeout,
            },
            on_timeout: file.on_timeout.unwrap_or(defaults.on_timeout),
            loop_limit: match file.loop_limit {
                Some(n) if LOOP_LIMITS.contains(&n) => n,
                Some(n) => return Err(PolicyError::BadLoopLimit(n)),
                None => defaults.loop_limit,
            },
        })
    }

    /// The profile's own rule for `class`, if it sets one.
    pub fn rule(&self, class: ActionClass) -> Option<Rule> {
        self.rules.get(&class).copied()
    }

    /// The profile as TOML that [`Profile::from_toml`] reads back the same (TP-PED-02): rules
    /// in [`CLASSES`] order, the timeout in whole hours, minutes or seconds.
    pub fn to_toml(&self) -> Result<String, PolicyError> {
        let file = ProfileOut {
            sandbox_roots: &self.sandbox_roots,
            net_allow: &self.net_allow,
            checkpoint_timeout: duration_text(self.checkpoint_timeout),
            on_timeout: self.on_timeout,
            loop_limit: self.loop_limit,
            rules: CLASSES
                .iter()
                .filter_map(|class| {
                    self.rule(*class).map(|decision| RuleOut {
                        class: *class,
                        decision,
                    })
                })
                .collect(),
        };
        toml::to_string(&file).map_err(|e| PolicyError::Write(e.to_string()))
    }
}

/// A profile as it's written back.
#[derive(Serialize)]
struct ProfileOut<'a> {
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    sandbox_roots: &'a [String],
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    net_allow: &'a [String],
    checkpoint_timeout: String,
    on_timeout: OnTimeout,
    loop_limit: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    rules: Vec<RuleOut>,
}

#[derive(Serialize)]
struct RuleOut {
    class: ActionClass,
    decision: Rule,
}

/// `2h`, `45m` or `90s`: the largest unit that divides it.
fn duration_text(duration: Duration) -> String {
    let s = duration.as_secs();
    if s.is_multiple_of(3600) {
        format!("{}h", s / 3600)
    } else if s.is_multiple_of(60) {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

/// Every action class, in the order the policy editor shows them (TP-PED-01).
pub const CLASSES: [ActionClass; 13] = [
    ActionClass::ReadLocal,
    ActionClass::ReadLocalOutside,
    ActionClass::WriteLocal,
    ActionClass::DeleteLocal,
    ActionClass::ExecShell,
    ActionClass::NetRead,
    ActionClass::NetReadUnlisted,
    ActionClass::NetWrite,
    ActionClass::CommsSend,
    ActionClass::MoneySpend,
    ActionClass::BudgetThreshold,
    ActionClass::ProviderTools,
    ActionClass::Unknown,
];

/// Whether the policy editor changes `class`: not `unknown` (always denied), nor
/// `provider_tools` (switched in the agent editor).
pub fn editable(class: ActionClass) -> bool {
    match class {
        ActionClass::Unknown | ActionClass::ProviderTools => false,
        ActionClass::ReadLocal
        | ActionClass::ReadLocalOutside
        | ActionClass::WriteLocal
        | ActionClass::DeleteLocal
        | ActionClass::ExecShell
        | ActionClass::NetRead
        | ActionClass::NetReadUnlisted
        | ActionClass::NetWrite
        | ActionClass::CommsSend
        | ActionClass::MoneySpend
        | ActionClass::BudgetThreshold => true,
    }
}

/// The most allowed domains a profile holds.
pub const MAX_NET_ALLOW: usize = 100;

/// Where a class's effective rule comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleSource {
    /// The default for every agent.
    Default,
    /// The agent's own rule.
    Profile,
    /// A global ceiling capped it.
    Ceiling,
    /// Always the same: `unknown` is denied.
    Fixed,
}

/// What an agent's policy does with one class, for the policy editor (TP-PED-01).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Effective {
    /// The class.
    pub class: ActionClass,
    /// What the policy decides.
    pub rule: Rule,
    /// Why.
    pub source: RuleSource,
    /// What it would be without the agent's own rule.
    pub default: Rule,
    /// The most any agent may have.
    pub ceiling: Rule,
    /// Whether the policy editor changes it.
    pub editable: bool,
}

/// A policy editor's form (TP-PED-03): rule changes (`None` goes back to the default), and the
/// whole of each setting. [`Policy::edited`] checks it against the ceilings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileEdit {
    /// Rules to set or clear; the others stay.
    pub rules: Vec<(ActionClass, Option<Rule>)>,
    /// Domains `net.read` may reach without asking.
    pub net_allow: Vec<String>,
    /// Like `30m`, `90s` or `2h`.
    pub checkpoint_timeout: String,
    /// What an unanswered checkpoint does.
    pub on_timeout: OnTimeout,
    /// From this many identical calls in a run on, each asks.
    pub loop_limit: u32,
}

impl From<&Profile> for ProfileEdit {
    /// The edit that changes nothing.
    fn from(profile: &Profile) -> Self {
        Self {
            rules: Vec::new(),
            net_allow: profile.net_allow.clone(),
            checkpoint_timeout: duration_text(profile.checkpoint_timeout),
            on_timeout: profile.on_timeout,
            loop_limit: profile.loop_limit,
        }
    }
}

/// `raw` as an allowed domain: trimmed and lowercased, a hostname of at least two labels (letters,
/// digits and inner hyphens, at most 63 each, 253 in all, not all digits at the end), optionally
/// after one leading `*.` with at least two labels after it. Non-ASCII must be punycode
/// (security model T20).
fn domain(raw: &str) -> Result<String, PolicyError> {
    let bad = || PolicyError::BadDomain(raw.to_owned());
    let text = raw.trim().to_ascii_lowercase();
    let host = text.strip_prefix("*.").unwrap_or(&text);
    let labels: Vec<&str> = host.split('.').collect();
    let label_ok = |label: &&str| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    let top_ok = labels
        .last()
        .is_some_and(|top| !top.bytes().all(|b| b.is_ascii_digit()));
    if host.len() > 253 || labels.len() < 2 || !labels.iter().all(label_ok) || !top_ok {
        return Err(bad());
    }
    Ok(text)
}

/// `30m`, `90s`, `2h`: from one second to a week.
fn parse_duration(text: &str) -> Result<Duration, PolicyError> {
    let bad = || PolicyError::BadDuration(text.to_owned());
    let (number, unit) = text.split_at(text.len().saturating_sub(1));
    let n: u64 = number.parse().map_err(|_| bad())?;
    let seconds = match unit {
        "s" => Some(n),
        "m" => n.checked_mul(60),
        "h" => n.checked_mul(3600),
        _ => None,
    }
    .ok_or_else(bad)?;
    if seconds == 0 || seconds > 168 * 3600 {
        return Err(bad());
    }
    Ok(Duration::from_secs(seconds))
}

/// The most any profile may allow per class. Set globally, never per agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ceilings {
    caps: HashMap<ActionClass, Rule>,
}

impl Default for Ceilings {
    /// Money, deleting and running commands always ask at least; `unknown` is always denied.
    fn default() -> Self {
        Self {
            caps: HashMap::from([
                (ActionClass::MoneySpend, Rule::Ask),
                (ActionClass::DeleteLocal, Rule::Ask),
                (ActionClass::ExecShell, Rule::Ask),
                (ActionClass::Unknown, Rule::Deny),
            ]),
        }
    }
}

impl Ceilings {
    /// Lowers the ceiling for `class` to `rule` (never raises it).
    pub fn cap(mut self, class: ActionClass, rule: Rule) -> Self {
        let current = self.ceiling(class);
        self.caps.insert(class, current.min(rule));
        self
    }

    /// The most a profile may allow for `class`.
    pub fn ceiling(&self, class: ActionClass) -> Rule {
        self.caps.get(&class).copied().unwrap_or(Rule::Allow)
    }
}

/// What every agent gets unless its profile says otherwise (docs/01 §5).
pub fn default_rule(class: ActionClass) -> Rule {
    match class {
        ActionClass::ReadLocal | ActionClass::NetRead => Rule::Allow,
        ActionClass::ReadLocalOutside
        | ActionClass::WriteLocal
        | ActionClass::DeleteLocal
        | ActionClass::ExecShell
        | ActionClass::NetReadUnlisted
        | ActionClass::NetWrite
        | ActionClass::CommsSend
        | ActionClass::MoneySpend
        | ActionClass::BudgetThreshold => Rule::Ask,
        // Provider-side tools are decided before a run and are off unless the profile allows
        // them; `unknown` is never allowed.
        ActionClass::ProviderTools | ActionClass::Unknown => Rule::Deny,
    }
}

/// How much is at stake in an action.
pub fn risk_of(action: &Action) -> Risk {
    match action.class {
        ActionClass::ReadLocal
        | ActionClass::ReadLocalOutside
        | ActionClass::WriteLocal
        | ActionClass::NetRead
        | ActionClass::NetReadUnlisted
        | ActionClass::ProviderTools
        | ActionClass::BudgetThreshold => Risk::Normal,
        ActionClass::DeleteLocal if action.outside_sandbox => Risk::Critical,
        ActionClass::DeleteLocal
        | ActionClass::ExecShell
        | ActionClass::NetWrite
        | ActionClass::CommsSend => Risk::High,
        ActionClass::MoneySpend | ActionClass::Unknown => Risk::Critical,
    }
}

/// The ways a checkpoint can be answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    /// A click on Approve.
    Click,
    /// Return on the focused Approve button.
    Keyboard,
    /// A spoken "approve".
    Voice,
    /// Touch ID.
    TouchId,
}

/// Whether `how` may approve a checkpoint of `risk` (TP-POL-09).
pub fn can_approve(risk: Risk, how: Approval) -> bool {
    match (risk, how) {
        (
            Risk::Normal,
            Approval::Click | Approval::Keyboard | Approval::Voice | Approval::TouchId,
        )
        | (Risk::High, Approval::Click | Approval::Keyboard | Approval::TouchId)
        | (Risk::Critical, Approval::TouchId) => true,
        (Risk::High, Approval::Voice)
        | (Risk::Critical, Approval::Click | Approval::Keyboard | Approval::Voice) => false,
    }
}

/// The classes that can send data out of the Mac (T28).
fn is_way_out(class: ActionClass) -> bool {
    match class {
        ActionClass::NetWrite | ActionClass::CommsSend | ActionClass::NetReadUnlisted => true,
        ActionClass::ReadLocal
        | ActionClass::ReadLocalOutside
        | ActionClass::WriteLocal
        | ActionClass::DeleteLocal
        | ActionClass::ExecShell
        | ActionClass::NetRead
        | ActionClass::MoneySpend
        | ActionClass::ProviderTools
        | ActionClass::BudgetThreshold
        | ActionClass::Unknown => false,
    }
}

/// The checkpoint title for an action of `class`, e.g. "Sending a message or email". Written by the
/// core, never by the agent (security model T20).
pub fn title(class: ActionClass) -> &'static str {
    what(class)
}

/// What an action of `class` does, for checkpoint text.
fn what(class: ActionClass) -> &'static str {
    match class {
        ActionClass::ReadLocal => "Reading a file in its folder",
        ActionClass::ReadLocalOutside => "Reading a file outside its folder",
        ActionClass::WriteLocal => "Changing a file in its folder",
        ActionClass::DeleteLocal => "Deleting a file",
        ActionClass::ExecShell => "Running a command",
        ActionClass::NetRead => "Reading an allowed website",
        ActionClass::NetReadUnlisted => "Reading a website that isn't on its list",
        ActionClass::NetWrite => "Sending data to a website",
        ActionClass::CommsSend => "Sending a message or email",
        ActionClass::MoneySpend => "Spending money",
        ActionClass::ProviderTools => "Using the provider's own tools",
        ActionClass::BudgetThreshold => "Going past the spend limit",
        ActionClass::Unknown => "An action Corral doesn't recognize",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Grant {
    class: ActionClass,
    /// For high and critical classes: the only destination the grant covers.
    destination: Option<String>,
}

/// One run's "allow for the rest of this run" grants and taint. Lives with the run and is never
/// saved, so nothing carries over to the next run (TP-POL-04).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunGrants {
    grants: Vec<Grant>,
    tainted_by: Option<String>,
    /// Normal-risk calls the user approved, by fingerprint (TP-POL-08).
    approved: HashSet<u64>,
}

impl RunGrants {
    /// Records "allow for the rest of this run" for `action`. For high and critical classes it
    /// covers only the action's destination; without one it covers nothing.
    pub fn allow_for_run(&mut self, action: &Action) {
        let destination = match risk_of(action) {
            Risk::Normal => None,
            Risk::High | Risk::Critical => match &action.destination {
                Some(d) => Some(d.to_ascii_lowercase()),
                None => return,
            },
        };
        self.grants.push(Grant {
            class: action.class,
            destination,
        });
    }

    /// Marks the run as having read untrusted content from `source`. The first source stays.
    pub fn taint(&mut self, source: impl Into<String>) {
        if self.tainted_by.is_none() {
            self.tainted_by = Some(source.into());
        }
    }

    /// The first untrusted source the run read, if any.
    pub fn tainted_by(&self) -> Option<&str> {
        self.tainted_by.as_deref()
    }

    fn covers(&self, action: &Action) -> bool {
        let destination = action.destination.as_ref().map(|d| d.to_ascii_lowercase());
        self.grants.iter().any(|g| {
            g.class == action.class
                && match &g.destination {
                    None => true,
                    Some(pinned) => destination.as_ref() == Some(pinned),
                }
        })
    }
}

/// A profile under the global ceilings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    profile: Profile,
    ceilings: Ceilings,
}

impl Policy {
    /// `profile` under the default ceilings.
    pub fn new(profile: Profile) -> Self {
        Self::with_ceilings(profile, Ceilings::default())
    }

    /// `profile` under `ceilings`.
    pub fn with_ceilings(profile: Profile, ceilings: Ceilings) -> Self {
        Self { profile, ceilings }
    }

    /// The profile.
    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// The global ceilings.
    pub fn ceilings(&self) -> &Ceilings {
        &self.ceilings
    }

    /// `profile` under the default ceilings, if it's one the policy editor could have saved
    /// (TP-PED-05): no rule above its ceiling, and every allowed domain a plain one. A saved
    /// file that isn't is refused rather than capped or cleaned, so it never quietly changes.
    pub fn checked(profile: Profile) -> Result<Policy, PolicyError> {
        let ceilings = Ceilings::default();
        for class in CLASSES {
            if let Some(rule) = profile.rule(class) {
                let ceiling = ceilings.ceiling(class);
                if rule > ceiling {
                    return Err(PolicyError::AboveCeiling { class, ceiling });
                }
            }
        }
        if profile.net_allow.len() > MAX_NET_ALLOW {
            return Err(PolicyError::TooManyDomains(profile.net_allow.len()));
        }
        for entry in &profile.net_allow {
            if domain(entry)? != *entry {
                return Err(PolicyError::BadDomain(entry.clone()));
            }
        }
        Ok(Policy::with_ceilings(profile, ceilings))
    }

    /// This policy with provider-side tools allowed (TP-PROV-14): the agent editor's switch.
    /// Everything else stays.
    pub fn allowing_provider_tools(&self) -> Policy {
        let mut profile = self.profile.clone();
        profile
            .rules
            .insert(ActionClass::ProviderTools, Rule::Allow);
        Policy::with_ceilings(profile, self.ceilings.clone())
    }

    /// What this policy does with `class`, and why (TP-PED-01): the same rule [`Policy::decide`]
    /// applies before any grant.
    pub fn effective(&self, class: ActionClass) -> Effective {
        let default = default_rule(class);
        let ceiling = self.ceilings.ceiling(class);
        let own = self.profile.rule(class);
        let wanted = own.unwrap_or(default);
        let (rule, source) = if class == ActionClass::Unknown {
            (Rule::Deny, RuleSource::Fixed)
        } else if wanted > ceiling {
            (ceiling, RuleSource::Ceiling)
        } else if own.is_some() {
            (wanted, RuleSource::Profile)
        } else {
            (wanted, RuleSource::Default)
        };
        Effective {
            class,
            rule,
            source,
            default,
            ceiling,
            editable: editable(class),
        }
    }

    /// This policy with `edit` applied, under the same ceilings, or why it can't be (TP-PED-03):
    /// a rule above its ceiling is refused rather than capped, so what's saved is what applies.
    pub fn edited(&self, edit: &ProfileEdit) -> Result<Policy, PolicyError> {
        let mut profile = self.profile.clone();
        for (class, rule) in &edit.rules {
            if !editable(*class) {
                return Err(PolicyError::NotEditable(*class));
            }
            match rule {
                Some(rule) => {
                    let ceiling = self.ceilings.ceiling(*class);
                    if *rule > ceiling {
                        return Err(PolicyError::AboveCeiling {
                            class: *class,
                            ceiling,
                        });
                    }
                    profile.rules.insert(*class, *rule);
                }
                None => {
                    profile.rules.remove(class);
                }
            }
        }
        if edit.net_allow.len() > MAX_NET_ALLOW {
            return Err(PolicyError::TooManyDomains(edit.net_allow.len()));
        }
        let mut net_allow: Vec<String> = Vec::new();
        for raw in &edit.net_allow {
            let domain = domain(raw)?;
            if !net_allow.contains(&domain) {
                net_allow.push(domain);
            }
        }
        profile.net_allow = net_allow;
        profile.checkpoint_timeout = parse_duration(edit.checkpoint_timeout.trim())?;
        profile.on_timeout = edit.on_timeout;
        if !LOOP_LIMITS.contains(&edit.loop_limit) {
            return Err(PolicyError::BadLoopLimit(edit.loop_limit));
        }
        profile.loop_limit = edit.loop_limit;
        Ok(Policy::with_ceilings(profile, self.ceilings.clone()))
    }

    /// Decides one action (ADR-003). Unknown is denied first; then the profile's rule (or the
    /// default) capped by the ceiling; an Ask becomes Allow only through a grant this run holds,
    /// and never for a way out on a tainted run.
    pub fn decide(&self, action: &Action, run: &RunGrants) -> Decision {
        if action.class == ActionClass::Unknown {
            return Decision::Deny {
                reason: "Corral doesn't know what this action does, so it's blocked.".into(),
            };
        }
        let rule = self
            .profile
            .rule(action.class)
            .unwrap_or_else(|| default_rule(action.class))
            .min(self.ceilings.ceiling(action.class));
        match rule {
            Rule::Deny => Decision::Deny {
                reason: format!("{} is blocked for this agent.", what(action.class)),
            },
            Rule::Allow => Decision::Allow,
            Rule::Ask => {
                let risk = risk_of(action);
                if is_way_out(action.class)
                    && let Some(source) = run.tainted_by()
                {
                    return Decision::Ask {
                        risk,
                        reason: format!(
                            "{} needs your approval: this run has read untrusted content ({source}).",
                            what(action.class)
                        ),
                    };
                }
                if run.covers(action) {
                    Decision::Allow
                } else {
                    Decision::Ask {
                        risk,
                        reason: format!("{} needs your approval.", what(action.class)),
                    }
                }
            }
        }
    }
}

/// One tool call an agent wants to make: what it is (for the policy) and its arguments (for the
/// tool).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// The action, mapped to its class.
    pub action: Action,
    /// The tool's arguments.
    pub args: serde_json::Value,
}

/// How long an approval lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// This call only.
    Once,
    /// This call, and the same class for the rest of the run (pinned to the destination for high
    /// and critical classes).
    ForRun,
}

/// Permission to run exactly one tool call. Only [`Gate::check`] and an approved
/// [`PendingApproval`] make one; it can't be built, cloned or reused (the tool gateway takes it by
/// value).
#[derive(Debug)]
pub struct AllowToken {
    call: ToolCall,
    run: Arc<Mutex<RunGrants>>,
}

impl AllowToken {
    /// The call this token allows.
    pub fn call(&self) -> &ToolCall {
        &self.call
    }

    /// Marks the token's run as having read untrusted content from `source` (T28). The tool
    /// gateway calls this after a tool that reads the web or an untrusted server.
    pub fn taint(&self, source: impl Into<String>) {
        lock(&self.run).taint(source);
    }
}

/// A checkpoint waiting for the user: the call, its risk and why it asks.
#[derive(Debug)]
pub struct PendingApproval {
    // Boxed so a refused approval, which hands this back, stays small.
    call: Box<ToolCall>,
    risk: Risk,
    reason: String,
    run: Arc<Mutex<RunGrants>>,
    /// How many identical calls the run has made, when that's why it asks.
    repeated: Option<u32>,
}

/// An approval the gesture wasn't enough for. The checkpoint stays open.
#[derive(Debug)]
pub struct Refused {
    /// The checkpoint, still waiting.
    pub pending: PendingApproval,
    /// Why, in plain words.
    pub reason: String,
}

/// Why an edited approval wasn't taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditRefusal {
    /// The gesture isn't enough for the risk, edited or not (TP-POL-09).
    Gesture,
    /// This checkpoint can't be edited: its arguments are the core's, not the agent's (a spend
    /// limit).
    NotEditable,
}

/// An edited approval that wasn't taken. The checkpoint stays open with the original call.
#[derive(Debug)]
pub struct EditRefused {
    /// The checkpoint, still waiting.
    pub pending: PendingApproval,
    /// Why.
    pub why: EditRefusal,
}

impl PendingApproval {
    /// How much is at stake.
    pub fn risk(&self) -> Risk {
        self.risk
    }

    /// Why it asks.
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// How many identical calls the run has made, when it asks because of that (the loop
    /// detector).
    pub fn repeated(&self) -> Option<u32> {
        self.repeated
    }

    /// The call waiting.
    pub fn call(&self) -> &ToolCall {
        &self.call
    }

    /// Approves with `how`. Refused (and still open) if the gesture isn't enough for the risk
    /// (TP-POL-09). With [`Scope::ForRun`] the run keeps a grant for later calls.
    pub fn approve(self, how: Approval, scope: Scope) -> Result<AllowToken, Refused> {
        if !can_approve(self.risk, how) {
            let reason = match self.risk {
                Risk::Critical => "This needs Touch ID.",
                Risk::High => "This can't be approved by voice.",
                Risk::Normal => "This approval wasn't accepted.",
            };
            return Err(Refused {
                pending: self,
                reason: reason.into(),
            });
        }
        {
            let mut run = lock(&self.run);
            if scope == Scope::ForRun {
                run.allow_for_run(&self.call.action);
            }
            if remembered(&self.call.action) {
                run.approved.insert(fingerprint(&self.call));
            }
        }
        Ok(AllowToken {
            call: *self.call,
            run: self.run,
        })
    }

    /// Whether the user may change this call's arguments before approving it. A spend-limit
    /// checkpoint can't be edited: its numbers are the core's.
    pub fn editable(&self) -> bool {
        self.call.action.class != ActionClass::BudgetThreshold
    }

    /// Approves the call with the user's edited `args` instead of the agent's (TP-POL-06). The
    /// tool and its class can't change, so the risk and the gesture it needs stay the same;
    /// `destination` is the one the tool reads from `args`, never one the user or the model
    /// names. An edit approves this call only: the run gets no grant. Refused (and still open,
    /// with the original call) if the gesture isn't enough or the checkpoint isn't editable.
    pub fn approve_edited(
        self,
        how: Approval,
        args: Value,
        destination: Option<String>,
    ) -> Result<AllowToken, EditRefused> {
        if !self.editable() {
            return Err(EditRefused {
                pending: self,
                why: EditRefusal::NotEditable,
            });
        }
        if !can_approve(self.risk, how) {
            return Err(EditRefused {
                pending: self,
                why: EditRefusal::Gesture,
            });
        }
        let mut call = *self.call;
        call.action.destination = destination;
        call.args = args;
        Ok(AllowToken {
            call,
            run: self.run,
        })
    }

    /// Denies it. Nothing is granted; the call comes back so the agent can be told.
    pub fn deny(self) -> ToolCall {
        *self.call
    }
}

/// What the gate says about one call.
#[derive(Debug)]
pub enum Verdict {
    /// Go ahead with this token.
    Allow(AllowToken),
    /// Wait for the user.
    Ask(PendingApproval),
    /// Never; the agent is told why.
    Deny {
        /// Why, in plain words.
        reason: String,
    },
}

/// The policy for one run, with that run's grants and taint. Every tool call goes through
/// [`Gate::check`].
#[derive(Debug)]
pub struct Gate {
    policy: Policy,
    run: Arc<Mutex<RunGrants>>,
    /// Identical calls so far, by fingerprint (the loop detector).
    calls: Mutex<HashMap<u64, u32>>,
}

impl Gate {
    /// A gate for a new run: no grants, no taint.
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            run: Arc::default(),
            calls: Mutex::default(),
        }
    }

    /// Decides one call. From the profile's `loop_limit`-th identical call in the run on, a
    /// call the policy would allow or ask about asks because of the repetition (T9); a denied
    /// call stays denied.
    pub fn check(&self, call: ToolCall) -> Verdict {
        let print = fingerprint(&call);
        let seen = self.count(print);
        let looping = (seen >= self.policy.profile().loop_limit).then_some(seen);
        let decision = {
            let run = lock(&self.run);
            match self.policy.decide(&call.action, &run) {
                // The user approved this exact call already (TP-POL-08). A way out on a
                // tainted run asks again: decide said why.
                Decision::Ask { .. }
                    if remembered(&call.action)
                        && run.approved.contains(&print)
                        && !(is_way_out(call.action.class) && run.tainted_by().is_some()) =>
                {
                    Decision::Allow
                }
                decision @ (Decision::Allow | Decision::Ask { .. } | Decision::Deny { .. }) => {
                    decision
                }
            }
        };
        let ask = |risk: Risk, reason: String, call: ToolCall| {
            Verdict::Ask(PendingApproval {
                reason: match looping {
                    Some(n) => format!(
                        "{} has run with the same arguments {n} times in this run. Corral asks before it runs again, in case the agent is stuck in a loop.",
                        call.action.tool
                    ),
                    None => reason,
                },
                call: Box::new(call),
                risk,
                run: Arc::clone(&self.run),
                repeated: looping,
            })
        };
        match decision {
            Decision::Allow if looping.is_none() => Verdict::Allow(AllowToken {
                call,
                run: Arc::clone(&self.run),
            }),
            Decision::Allow => ask(risk_of(&call.action), String::new(), call),
            Decision::Ask { risk, reason } => ask(risk, reason, call),
            Decision::Deny { reason } => Verdict::Deny { reason },
        }
    }

    /// Counts the call with fingerprint `print` and returns how many identical calls the run
    /// has made, this one included.
    fn count(&self, print: u64) -> u32 {
        let mut calls = self
            .calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let seen = calls.entry(print).or_insert(0);
        *seen = seen.saturating_add(1);
        *seen
    }

    /// The profile this run is decided under (for its checkpoint timeout).
    pub fn profile(&self) -> &Profile {
        self.policy.profile()
    }

    /// The first untrusted source this run read, if any.
    pub fn tainted_by(&self) -> Option<String> {
        lock(&self.run).tainted_by().map(str::to_owned)
    }

    /// Marks this run as having read untrusted content from `source` when no tool call through
    /// the gate brought it in: a provider's web search, which runs on its servers (T28,
    /// TP-GATE-08). Taint only ever makes the run stricter, so this needs no token.
    pub fn taint(&self, source: impl Into<String>) {
        lock(&self.run).taint(source);
    }
}

/// Whether an approval of this exact call is remembered for the run: normal risk only, and
/// never a spend limit, whose numbers are the core's.
fn remembered(action: &Action) -> bool {
    risk_of(action) == Risk::Normal && action.class != ActionClass::BudgetThreshold
}

/// The same tool with the same arguments, whatever order their keys come in.
fn fingerprint(call: &ToolCall) -> u64 {
    let mut hasher = DefaultHasher::new();
    call.action.tool.hash(&mut hasher);
    canonical(&call.args, &mut hasher);
    hasher.finish()
}

/// Hashes `value` with object keys in sorted order, tagging each kind so `"1"` and `1` differ.
fn canonical(value: &Value, hasher: &mut DefaultHasher) {
    match value {
        Value::Null => 0_u8.hash(hasher),
        Value::Bool(b) => (1_u8, b).hash(hasher),
        Value::Number(n) => (2_u8, n.to_string()).hash(hasher),
        Value::String(s) => (3_u8, s).hash(hasher),
        Value::Array(items) => {
            (4_u8, items.len()).hash(hasher);
            for item in items {
                canonical(item, hasher);
            }
        }
        Value::Object(map) => {
            (5_u8, map.len()).hash(hasher);
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            for (key, item) in entries {
                key.hash(hasher);
                canonical(item, hasher);
            }
        }
    }
}

/// The run's grants. A poisoned lock means a panic while holding it; the grants are plain data,
/// so they're still sound to read and extend.
fn lock(run: &Mutex<RunGrants>) -> std::sync::MutexGuard<'_, RunGrants> {
    run.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
