//! The tool gateway: the only place agent tools run (docs/01 §4–5, ADR-003, T2). Every call needs
//! a policy `AllowToken`, and the gateway runs exactly the call the token was issued for.
//!
//! - **Registered tools only.** Each tool says its action class when it's registered. A token
//!   decided for another class is refused and nothing runs, so an adapter can't call a sending
//!   tool a "read" to slip it past the policy (TP-POL-10).
//! - **Output is capped for the model** at [`MODEL_OUTPUT_CAP`] bytes, cut on a character
//!   boundary with a marker; the full output is kept for the trace (T30).
//! - **Taint.** A tool that reads untrusted content (the web, an untrusted MCP server) names its
//!   source, and the gateway marks the run through the token (T28).
//!
//! - **Checkpoints.** [`ToolRunner`] takes an agent's call through the gate: an Ask waits at the
//!   [`CheckpointDesk`] until the user answers with a strong enough gesture, the profile's timeout
//!   passes, or the run is stopped (docs/01 §5).
//!
//! - **The filesystem sandbox** ([`fs`]): `fs.read`, `fs.write` and `fs.delete` reach only the
//!   agent's own folders, opened beneath a directory handle (M2 item 6).
//!
//! The MCP client comes later (M5 item 6).

mod checkpoint;
pub mod fs;
mod runner;

pub use checkpoint::{Answer, CheckpointDesk, DeskError, MAX_NOTE_CHARS};
pub use runner::{IdSource, MAX_DETAIL_CHARS, ToolAnswer, ToolRunner};

use corral_core::domain::ActionClass;
use corral_policy::AllowToken;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// The most tool output the model sees, in bytes (docs/01 §4: 16 KB of text by default).
pub const MODEL_OUTPUT_CAP: usize = 16 * 1024;

/// A boxed future, so tools can be async behind `dyn Tool`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Why a tool failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolError {
    /// The arguments aren't what the tool takes.
    #[error("the tool's arguments aren't valid: {0}")]
    BadArgs(String),
    /// The tool ran and failed.
    #[error("the tool failed: {0}")]
    Failed(String),
}

/// One tool an agent can use.
pub trait Tool: Send + Sync {
    /// Its name, as agents call it (e.g. `email.send`).
    fn name(&self) -> &str;
    /// What kind of side effect it has. Fixed at registration.
    fn class(&self) -> ActionClass;
    /// For a tool that reads untrusted content: where this call reads from.
    fn untrusted_source(&self, args: &Value) -> Option<String> {
        let _ = args; // Trusted by default; only readers of untrusted content override this.
        None
    }
    /// Where this call sends data or acts, as the policy should see it (an address, a domain, a
    /// path). Read from the arguments by the tool, never taken from the model's description.
    fn destination(&self, args: &Value) -> Option<String> {
        let _ = args; // Most tools have no destination worth pinning a grant to.
        None
    }
    /// Runs the call.
    fn call<'a>(&'a self, args: &'a Value) -> BoxFuture<'a, Result<String, ToolError>>;
}

/// Why the gateway didn't run a call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GatewayError {
    /// No tool has that name.
    #[error("there's no tool called {0}")]
    UnknownTool(String),
    /// A tool with that name is already registered.
    #[error("a tool called {0} is already registered")]
    Duplicate(String),
    /// A tool registered as `unknown`.
    #[error("{0} doesn't say what kind of action it is, so it can't be registered")]
    UnknownClass(String),
    /// The token was decided for a different class than the tool really is.
    #[error("{tool} is {registered:?}, but the call was decided as {decided:?}; nothing ran")]
    ClassMismatch {
        /// The tool.
        tool: String,
        /// What it really is.
        registered: ActionClass,
        /// What the policy decided on.
        decided: ActionClass,
    },
    /// The tool ran and failed.
    #[error(transparent)]
    Tool(#[from] ToolError),
}

/// What a tool returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    /// What the model sees: capped at [`MODEL_OUTPUT_CAP`], with a marker if cut.
    pub for_model: String,
    /// Everything, for the trace.
    pub full: String,
    /// Whether `for_model` was cut.
    pub truncated: bool,
}

/// The registered tools.
#[derive(Default)]
pub struct ToolGateway {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolGateway {
    /// An empty gateway.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a tool under its name. A name is taken once, with its class.
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<(), GatewayError> {
        let name = tool.name().to_owned();
        if tool.class() == ActionClass::Unknown {
            return Err(GatewayError::UnknownClass(name));
        }
        if self.tools.contains_key(&name) {
            return Err(GatewayError::Duplicate(name));
        }
        self.tools.insert(name, tool);
        Ok(())
    }

    /// A registered tool's class, so adapters map calls from the registry, not from the model.
    pub fn class_of(&self, name: &str) -> Option<ActionClass> {
        self.tools.get(name).map(|tool| tool.class())
    }

    /// Where a call to `name` with `args` goes, as the tool reads it.
    pub fn destination_of(&self, name: &str, args: &Value) -> Option<String> {
        self.tools.get(name).and_then(|tool| tool.destination(args))
    }

    /// Runs the call `token` allows, and nothing else.
    pub async fn run(&self, token: AllowToken) -> Result<ToolOutput, GatewayError> {
        let call = token.call();
        let name = &call.action.tool;
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| GatewayError::UnknownTool(name.clone()))?;
        if tool.class() != call.action.class {
            return Err(GatewayError::ClassMismatch {
                tool: name.clone(),
                registered: tool.class(),
                decided: call.action.class,
            });
        }
        let full = tool.call(&call.args).await?;
        if let Some(source) = tool.untrusted_source(&call.args) {
            token.taint(source);
        }
        Ok(capped(full))
    }
}

/// Caps output for the model on a character boundary, with a marker saying how much was cut.
fn capped(full: String) -> ToolOutput {
    if full.len() <= MODEL_OUTPUT_CAP {
        return ToolOutput {
            for_model: full.clone(),
            full,
            truncated: false,
        };
    }
    let mut cut = MODEL_OUTPUT_CAP;
    while !full.is_char_boundary(cut) {
        cut -= 1;
    }
    let for_model = format!(
        "{}\n[Output truncated: {} more bytes are in the trace.]",
        &full[..cut],
        full.len() - cut
    );
    ToolOutput {
        for_model,
        full,
        truncated: true,
    }
}
