//! The `AgentAdapter` trait and every adapter: native, managed, ACP, A2A, fake.
//!
//! Spec: `docs/01-system-design.md §4`. The walking skeleton's adapter is [`responses`]: the
//! OpenAI Responses protocol, used by xAI's Grok (SK-4), text only, no tools.
//!
//! - [`sse`]: server-sent events, parsed the same however the network splits them.
//! - [`transport`]: one HTTP POST with a streamed body; the network lives only here.
//! - [`cassette`]: recorded exchanges replayed in tests, so tests never call the network
//!   (AGENTS.md rule 7).

pub mod cassette;
pub mod responses;
pub mod sse;
pub mod transport;
