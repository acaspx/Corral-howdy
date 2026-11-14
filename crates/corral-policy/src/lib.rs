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
//! - **Taint** (T28): once a run has read untrusted content, grants on the classes that can send
//!   data out (`net.write`, `comms.send`, `net.read.unlisted`) stop applying, and the checkpoint
//!   names the untrusted source. Taint never clears within a run.

use corral_core::domain::{ActionClass, Risk};
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;

/// What a profile or ceiling says about a class, from strictest to most permissive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
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
}

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
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            sandbox_roots: Vec::new(),
            net_allow: Vec::new(),
            rules: HashMap::new(),
            checkpoint_timeout: Duration::from_secs(30 * 60),
            on_timeout: OnTimeout::Deny,
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
        })
    }

    /// The profile's own rule for `class`, if it sets one.
    pub fn rule(&self, class: ActionClass) -> Option<Rule> {
        self.rules.get(&class).copied()
    }
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
