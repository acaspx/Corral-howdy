//! Shared domain vocabulary: action classes and risk tiers (system design §5).

use serde::{Deserialize, Serialize};

/// What kind of side effect an action has. Every adapter maps each action to exactly one class,
/// and the policy engine decides per class. Match on it exhaustively (AGENTS.md rule 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ActionClass {
    /// Read a file inside the agent's sandbox roots.
    #[serde(rename = "read.local")]
    ReadLocal,
    /// Read a file outside the sandbox roots.
    #[serde(rename = "read.local.outside")]
    ReadLocalOutside,
    /// Create or modify a file inside the sandbox roots.
    #[serde(rename = "write.local")]
    WriteLocal,
    /// Delete any file.
    #[serde(rename = "delete.local")]
    DeleteLocal,
    /// Run a command.
    #[serde(rename = "exec.shell")]
    ExecShell,
    /// HTTP GET to an allowlisted domain.
    #[serde(rename = "net.read")]
    NetRead,
    /// HTTP GET to a domain that isn't allowlisted.
    #[serde(rename = "net.read.unlisted")]
    NetReadUnlisted,
    /// POST, PUT, DELETE, form submits, webhooks.
    #[serde(rename = "net.write")]
    NetWrite,
    /// Email, messages, posts.
    #[serde(rename = "comms.send")]
    CommsSend,
    /// Purchases and payments.
    #[serde(rename = "money.spend")]
    MoneySpend,
    /// Tools that run on the provider's side, decided before the run starts.
    #[serde(rename = "provider_tools")]
    ProviderTools,
    /// Spend crosses a soft limit.
    #[serde(rename = "budget.threshold")]
    BudgetThreshold,
    /// Anything an adapter can't map. Always denied.
    #[serde(rename = "unknown")]
    Unknown,
}

/// How much is at stake in an action; it sets the gesture needed to approve it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    /// Click or keyboard.
    Normal,
    /// Click on a pinned card; voice can't approve.
    High,
    /// Touch ID.
    Critical,
}
