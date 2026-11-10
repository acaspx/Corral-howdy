//! Identifier newtypes. They're plain strings on the wire; the types keep a run ID from being
//! passed where a span ID belongs. Callers (the store, the supervisor, tests) choose the values, so
//! IDs are deterministic wherever a test needs them to be.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// The identifier as a string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_type!(
    /// One deployment of one agent on one task.
    RunId
);
id_type!(
    /// A conversation that survives runs: "prompt again" adds a run to the same session.
    SessionId
);
id_type!(
    /// An agent definition in the corral.
    AgentId
);
id_type!(
    /// A trace span (OpenTelemetry GenAI conventions: `invoke_agent`, `chat`, `execute_tool`).
    SpanId
);
id_type!(
    /// A pending human decision.
    CheckpointId
);
id_type!(
    /// One tool call requested by an agent, from request to result.
    ToolCallId
);
