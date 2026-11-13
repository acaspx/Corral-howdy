//! The `AgentAdapter` trait and every adapter: native, managed, ACP, A2A, fake.
//!
//! Spec: `docs/01-system-design.md §4`. The walking skeleton's adapters are text only, no tools:
//! [`responses`], the OpenAI Responses protocol, for xAI's Grok (SK-4), and [`messages`],
//! Anthropic's Messages protocol, for Claude (SK-7). Both run on [`stream`].
//!
//! - [`sse`]: server-sent events, parsed the same however the network splits them.
//! - [`transport`]: one HTTP POST with a streamed body; the network lives only here.
//! - [`catalog`]: the agent each provider's key brings, and the router that runs it.
//! - [`verify`]: checks a key with its provider before it's stored (TP-SETUP-02).
//! - [`cassette`]: recorded exchanges replayed in tests, so tests never call the network
//!   (AGENTS.md rule 7).

pub mod cassette;
pub mod catalog;
pub mod logos;
pub mod messages;
pub mod responses;
pub mod sse;
pub mod stream;
pub mod transport;
pub mod verify;
