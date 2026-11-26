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
//! - [`smoke`]: what the nightly live check passes and fails on (EV-PROV-01, 03).
//! - [`cassette`]: recorded exchanges replayed in tests, so tests never call the network
//!   (AGENTS.md rule 7).

pub mod agent;
pub mod cassette;
pub mod catalog;
pub mod chatgpt;
pub mod fake;
pub mod linked;
pub mod logos;
pub mod messages;
pub mod responses;
pub mod smoke;
pub mod sse;
pub mod stream;
pub mod transport;
pub mod verify;
