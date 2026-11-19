//! Anthropic's Messages protocol (`POST /v1/messages`, streamed), for Claude (SK-7), with the same
//! contract as [`crate::responses`]:
//!
//! - **No tools unless offered** (TP-SKEL-02): the plain body is the model, `max_tokens`,
//!   `stream` and one user message; no `tools`, `tool_choice`, `mcp_servers` or system prompt. A
//!   content block that is any kind of tool use (`tool_use`, `server_tool_use`, `mcp_tool_use`…)
//!   fails the run with [`FailureReason::ToolsNotEnabled`]. Thinking blocks are read and not
//!   shown; other block kinds Anthropic adds later are ignored, as its versioning policy asks.
//! - **With tools** (TP-TOOL-16..18): [`MessagesConfig::turn_body`] offers the agent's tools
//!   (`name`, `description`, `input_schema`), one call at a time
//!   (`disable_parallel_tool_use`), and carries the whole [`MessagesConversation`]: each turn
//!   goes back as an assistant message with its text and `tool_use` blocks, then a user message
//!   with a `tool_result` for each. [`MessageDecoder::offering_tools`] assembles each `tool_use`
//!   block from its streamed JSON pieces and leaves the run going for the agent loop. The
//!   assistant's blocks go back exactly as they came ([`MessageDecoder::content`]), which
//!   Anthropic requires once a search is in them.
//! - **Web search** (M3 item 4, TP-PROV-01..09): offered only when [`MessagesConfig::web_search`]
//!   is on, which the agent loop allows only under a policy that allows `provider_tools`. It's
//!   the basic version ([`WEB_SEARCH_TOOL`]): the newer ones run code at Anthropic by default.
//!   At most [`WEB_SEARCHES_PER_TURN`] a turn, each counted in the budget at
//!   [`WEB_SEARCH_MICRO_USD`]. Each search shows in the trace (`tool.requested` with class
//!   `provider_tools`, then its result's titles and links), its results taint the run, the answer
//!   lists the sources it cites, and a turn Anthropic pauses (`pause_turn`) is resumed. Other
//!   tools that run at Anthropic (code execution, MCP, web fetch) are still refused.
//! - **The key** is read at request time and sent in `x-api-key`, with
//!   `anthropic-version: 2023-06-01`.
//! - **Usage** (TP-ADP-05): input is `input_tokens` plus cache writes and cache reads, all priced
//!   at the full input rate; output is `message_delta`'s running total. Anthropic reports no
//!   cost, so it's computed from tokens at [`MessagesConfig::price`], rounded up.
//! - **The hard spend cap** (TP-BUD-01, 05): `max_tokens` is what the budget meter allows, never
//!   more than [`MessagesConfig::max_output_tokens`].
//! - **Errors** (TP-ADP-04): HTTP errors as in [`crate::stream`] (429, 503 and 529 retried); an
//!   `error` event mid-stream is a provider failure with Anthropic's message.

use crate::agent::{FunctionCall, MAX_CALL_ARGUMENT_BYTES, ProviderTool, ToolProtocol};
use crate::sse::SseEvent;
use crate::stream::{Protocol, StreamDecoder, StreamDriver, clean};
use crate::transport::KeyHeader;
use corral_core::budget::{Price, RequestEstimate, input_token_bound};
use corral_core::domain::ActionClass;
use corral_core::event::{
    EventBody, FailureReason, ModelDelta, ModelResponse, PolicyDecided, PolicyDecision,
    RunCompleted, RunFailed, ToolRequested, ToolResult, ToolStarted, UsageReported,
};
use corral_core::ids::ToolCallId;
use corral_secrets::Provider;
use corral_tools::OfferedTool;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;

/// The `anthropic-version` Corral speaks.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Anthropic's basic web search. Later versions filter results by running code at Anthropic,
/// which is a side-effecting provider tool, so Corral doesn't offer them.
pub const WEB_SEARCH_TOOL: &str = "web_search_20250305";

/// The most searches Claude may make in one turn.
pub const WEB_SEARCHES_PER_TURN: u32 = 5;

/// What one search costs: $10 per 1,000 (checked November 2026), in micro-USD.
pub const WEB_SEARCH_MICRO_USD: u64 = 10_000;

/// How a run tainted by search results names them in a checkpoint.
const SEARCH_RESULTS: &str = "web search results from Anthropic's servers";

/// The most sources an answer lists.
const MAX_SOURCES: usize = 10;

/// The most results of one search shown in the trace.
const MAX_RESULTS_SHOWN: usize = 20;

/// The longest link kept, in characters.
const MAX_URL_CHARS: usize = 2_000;

/// Where and how to call a Messages API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagesConfig {
    /// Whose key to use, and the name in messages.
    pub provider: Provider,
    /// API base, e.g. `https://api.anthropic.com/v1`.
    pub base_url: String,
    /// Model ID (configuration, never hard-coded in callers).
    pub model: String,
    /// Upper bound on output tokens per request, thinking included. The budget meter may lower
    /// it for a request; it never raises it.
    pub max_output_tokens: u32,
    /// The model's price, for the budget meter and the reported cost.
    pub price: Price,
    /// Attempts in all for a rate-limited or overloaded request.
    pub max_attempts: u32,
    /// Fail the run if the stream sends nothing for this long.
    pub idle_timeout: Duration,
    /// Offer Anthropic's web search to an agent with tools. Off by default; the agent loop turns
    /// it off unless the agent's policy allows `provider_tools`. Never sent without tools.
    pub web_search: bool,
}

impl MessagesConfig {
    /// Anthropic's API with the given model, priced at Claude Sonnet 5.5's list price ($2 per
    /// million input tokens, $10 per million output tokens, checked November 2026). Set
    /// [`Self::price`] for another model; the versioned price table replaces this in M2.
    pub fn anthropic(model: impl Into<String>) -> Self {
        Self {
            provider: Provider::Anthropic,
            base_url: "https://api.anthropic.com/v1".into(),
            model: model.into(),
            max_output_tokens: 16_384,
            price: Price::per_million_tokens(2_000_000, 10_000_000),
            max_attempts: 3,
            idle_timeout: Duration::from_secs(300),
            web_search: false,
        }
    }

    /// The endpoint.
    pub fn url(&self) -> String {
        format!("{}/messages", self.base_url.trim_end_matches('/'))
    }

    /// The request body with the output limit the budget meter allowed: the prompt and nothing
    /// that could run anything (TP-SKEL-02).
    pub fn request_body_limited(&self, prompt: &str, max_tokens: u32) -> Value {
        json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "stream": true,
            "messages": [{"role": "user", "content": prompt}],
        })
    }

    /// The most this request could cost, for the budget meter.
    pub fn estimate(&self, prompt: &str) -> RequestEstimate {
        RequestEstimate {
            price: self.price,
            input_tokens: input_token_bound(prompt),
            // No server tools are sent (TP-SKEL-02), so none can be billed.
            tool_fees_micro_usd: 0,
            max_output_tokens: self.max_output_tokens,
        }
    }
}

/// Everything said so far in a run, as Messages API messages. Nothing is kept at Anthropic, so
/// each request carries it all.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MessagesConversation {
    messages: Vec<Value>,
}

impl MessagesConversation {
    /// A conversation that starts with the user's prompt.
    pub fn new(prompt: &str) -> Self {
        Self {
            messages: vec![json!({"role": "user", "content": prompt})],
        }
    }

    /// A turn: the model's text and its `tool_use` blocks, then a `tool_result` for each. Input
    /// that isn't a JSON object goes back as `{}`, since Anthropic accepts nothing else.
    pub fn turn(&mut self, said: &str, results: &[(FunctionCall, String)]) {
        let mut asked = Vec::with_capacity(results.len() + 1);
        if !said.is_empty() {
            asked.push(json!({"type": "text", "text": said}));
        }
        let mut answered = Vec::with_capacity(results.len());
        for (call, output) in results {
            let input = match serde_json::from_str::<Value>(&call.arguments) {
                Ok(object @ Value::Object(_)) => object,
                Ok(_) | Err(_) => json!({}),
            };
            asked.push(json!({
                "type": "tool_use",
                "id": call.call_id,
                "name": call.name,
                "input": input,
            }));
            answered.push(json!({
                "type": "tool_result",
                "tool_use_id": call.call_id,
                "content": output,
            }));
        }
        self.messages
            .push(json!({"role": "assistant", "content": asked}));
        self.messages
            .push(json!({"role": "user", "content": answered}));
    }

    /// A turn as the model sent it: its content blocks exactly as they came (text, citations,
    /// searches and their encrypted results, calls), then a `tool_result` for each call. With no
    /// calls to answer (a paused turn) nothing follows the assistant's message.
    pub fn reply(&mut self, content: &[Value], results: &[(FunctionCall, String)]) {
        if !content.is_empty() {
            self.messages
                .push(json!({"role": "assistant", "content": content}));
        }
        if results.is_empty() {
            return;
        }
        let answered: Vec<Value> = results
            .iter()
            .map(|(call, output)| {
                json!({
                    "type": "tool_result",
                    "tool_use_id": call.call_id,
                    "content": output,
                })
            })
            .collect();
        self.messages
            .push(json!({"role": "user", "content": answered}));
    }

    /// The messages, in order.
    pub fn messages(&self) -> &[Value] {
        &self.messages
    }
}

/// The tools as Messages API tools.
fn messages_tools(tools: &[OfferedTool]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.wire_name,
                "description": tool.description,
                "input_schema": tool.parameters,
            })
        })
        .collect()
}

impl MessagesConfig {
    /// Whether a turn with `tools` offers the search: it's on, and none of the agent's own tools
    /// already has its name.
    fn offers_search(&self, tools: &[OfferedTool]) -> bool {
        self.web_search
            && !tools
                .iter()
                .any(|tool| tool.wire_name == ProviderTool::WebSearch.name())
    }

    /// The agent's tools, then the search if it's offered.
    fn all_tools(&self, tools: &[OfferedTool]) -> Vec<Value> {
        let mut all = messages_tools(tools);
        if self.offers_search(tools) {
            all.push(json!({
                "type": WEB_SEARCH_TOOL,
                "name": ProviderTool::WebSearch.name(),
                "max_uses": WEB_SEARCHES_PER_TURN,
            }));
        }
        all
    }

    /// One turn's request: the whole conversation, the agent's tools and the search if it's on
    /// (none means nothing tool-shaped is sent), and the output limit the budget meter allowed.
    pub fn turn_body(
        &self,
        conversation: &MessagesConversation,
        tools: &[OfferedTool],
        max_tokens: u32,
    ) -> Value {
        let mut body = json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "stream": true,
            "messages": conversation.messages(),
        });
        let all = self.all_tools(tools);
        if !all.is_empty() {
            body["tools"] = Value::Array(all);
            body["tool_choice"] = json!({"type": "auto", "disable_parallel_tool_use": true});
        }
        body
    }

    /// The most one turn could cost: the conversation and the tools' descriptions count as input.
    pub fn turn_estimate(
        &self,
        conversation: &MessagesConversation,
        tools: &[OfferedTool],
    ) -> RequestEstimate {
        let input = json!({"messages": conversation.messages(), "tools": self.all_tools(tools)});
        let searches = if self.offers_search(tools) {
            u64::from(WEB_SEARCHES_PER_TURN)
        } else {
            0
        };
        RequestEstimate {
            price: self.price,
            input_tokens: input_token_bound(&input.to_string()),
            // Results add input tokens as well; reconciliation counts those.
            tool_fees_micro_usd: searches.saturating_mul(WEB_SEARCH_MICRO_USD),
            max_output_tokens: self.max_output_tokens,
        }
    }
}

impl ToolProtocol for MessagesConfig {
    type Conversation = MessagesConversation;
    type ToolDecoder = MessageDecoder;
    type Said = Vec<Value>;

    fn conversation(&self, prompt: &str) -> MessagesConversation {
        MessagesConversation::new(prompt)
    }

    fn turn_body(
        &self,
        conversation: &MessagesConversation,
        tools: &[OfferedTool],
        max: u32,
    ) -> Value {
        MessagesConfig::turn_body(self, conversation, tools, max)
    }

    fn turn_estimate(
        &self,
        conversation: &MessagesConversation,
        tools: &[OfferedTool],
    ) -> RequestEstimate {
        MessagesConfig::turn_estimate(self, conversation, tools)
    }

    fn tool_decoder(&self) -> MessageDecoder {
        let decoder = MessageDecoder::offering_tools(self.price);
        if self.web_search {
            decoder.with_web_search()
        } else {
            decoder
        }
    }

    fn take_turn(decoder: &mut MessageDecoder) -> (Vec<Value>, Vec<FunctionCall>) {
        (decoder.take_content(), decoder.take_calls())
    }

    fn record(
        conversation: &mut MessagesConversation,
        said: &Vec<Value>,
        results: &[(FunctionCall, String)],
    ) {
        conversation.reply(said, results);
    }

    fn paused(decoder: &MessageDecoder) -> bool {
        decoder.paused
    }

    fn untrusted(decoder: &MessageDecoder) -> Option<&'static str> {
        decoder.untrusted()
    }

    fn keep_provider_tools(&mut self, keep: &dyn Fn(ProviderTool) -> bool) {
        if self.web_search && !keep(ProviderTool::WebSearch) {
            self.web_search = false;
        }
    }
}

/// A [`corral_core::supervisor::RunDriver`] that runs a prompt through a Messages API.
pub type MessagesDriver = StreamDriver<MessagesConfig>;

impl Protocol for MessagesConfig {
    type Decoder = MessageDecoder;

    fn provider(&self) -> Provider {
        self.provider
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn estimate(&self, prompt: &str) -> RequestEstimate {
        MessagesConfig::estimate(self, prompt)
    }

    fn url(&self) -> String {
        MessagesConfig::url(self)
    }

    fn body(&self, prompt: &str, max_output_tokens: u32) -> Value {
        self.request_body_limited(prompt, max_output_tokens)
    }

    fn key_header(&self) -> KeyHeader {
        KeyHeader::Named("x-api-key")
    }

    fn headers(&self) -> &'static [(&'static str, &'static str)] {
        &[("anthropic-version", ANTHROPIC_VERSION)]
    }

    fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    fn decoder(&self) -> MessageDecoder {
        MessageDecoder::new(self.price)
    }
}

/// Turns Messages stream events into Corral events. Pure, so cassettes check it without a
/// network.
#[derive(Debug)]
pub struct MessageDecoder {
    price: Price,
    text: String,
    input_tokens: u64,
    output_tokens: u64,
    /// Searches Anthropic ran, as its running total reports them.
    searches: u64,
    stop_reason: Option<String>,
    finished: bool,
    /// The agent was offered tools, so its `tool_use` blocks are read rather than refused.
    tools: bool,
    /// The agent was offered the search, so its blocks are read rather than refused.
    web_search: bool,
    /// `tool_use` blocks still streaming, by block index.
    open: BTreeMap<u64, FunctionCall>,
    /// Searches still streaming their query, by block index: the search's ID and the JSON so far.
    querying: BTreeMap<u64, (String, String)>,
    calls: Vec<FunctionCall>,
    /// The reply's content blocks as they came, by block index.
    blocks: BTreeMap<u64, Value>,
    /// The same blocks in order, once the response is over.
    content: Vec<Value>,
    /// The sources the answer cites: title and link, each once.
    sources: Vec<(String, String)>,
    /// A search brought back results.
    read_results: bool,
    /// Anthropic paused the turn; it goes back as it is.
    paused: bool,
    /// The reply's whole text, once the response is over.
    said: String,
}

impl MessageDecoder {
    /// A decoder that prices usage at `price`.
    pub fn new(price: Price) -> Self {
        Self {
            price,
            text: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            searches: 0,
            stop_reason: None,
            finished: false,
            tools: false,
            web_search: false,
            open: BTreeMap::new(),
            querying: BTreeMap::new(),
            calls: Vec::new(),
            blocks: BTreeMap::new(),
            content: Vec::new(),
            sources: Vec::new(),
            read_results: false,
            paused: false,
            said: String::new(),
        }
    }

    /// A decoder for a request that offered tools: `tool_use` blocks are kept for the loop.
    pub fn offering_tools(price: Price) -> Self {
        Self {
            tools: true,
            ..Self::new(price)
        }
    }

    /// Also reads Anthropic's web search, for a request that offered it.
    pub fn with_web_search(mut self) -> Self {
        self.web_search = true;
        self
    }

    /// The reply's whole text, once the response is over (empty before). Without the sources
    /// the answer lists.
    pub fn said(&self) -> &str {
        &self.said
    }

    /// The tool calls the response made, in order. Each is handed out once.
    pub fn take_calls(&mut self) -> Vec<FunctionCall> {
        std::mem::take(&mut self.calls)
    }

    /// The reply's content blocks exactly as they came, in order, once the response is over.
    pub fn content(&self) -> &[Value] {
        &self.content
    }

    /// The content blocks, handed out once.
    pub fn take_content(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.content)
    }

    /// What untrusted content the response read: search results, if a search brought any.
    pub fn untrusted(&self) -> Option<&'static str> {
        self.read_results.then_some(SEARCH_RESULTS)
    }

    fn fail(&mut self, reason: FailureReason, message: String) -> Vec<EventBody> {
        self.finished = true;
        self.open.clear();
        self.querying.clear();
        self.calls.clear();
        self.blocks.clear();
        self.content.clear();
        vec![EventBody::RunFailed(RunFailed { reason, message })]
    }

    /// Fails the run over a tool the agent wasn't given. Nothing ran.
    fn refuse_tool(&mut self, tool: &str) -> Vec<EventBody> {
        let message = if self.tools {
            format!(
                "The model asked to use a tool Corral didn't give it ({}). Nothing ran.",
                clean(tool)
            )
        } else {
            format!(
                "The model asked to use a tool ({}), but this agent has no tools yet. Nothing ran.",
                clean(tool)
            )
        };
        self.fail(FailureReason::ToolsNotEnabled, message)
    }

    /// Opens a `tool_use` block, or fails the run if it can't be used.
    fn open_call(&mut self, index: u64, block: &Value) -> Vec<EventBody> {
        let (call_id, name) = (text_of(block, "id"), text_of(block, "name"));
        if call_id.is_empty() || name.is_empty() {
            return self.fail(
                FailureReason::Adapter,
                "the model made a tool call without a name or an ID".into(),
            );
        }
        self.open.insert(
            index,
            FunctionCall {
                call_id,
                name,
                arguments: String::new(),
            },
        );
        self.blocks.insert(index, block.clone());
        Vec::new()
    }

    /// Opens a search block. Its query streams in, or comes whole in the block.
    fn open_search(&mut self, index: u64, block: &Value) -> Vec<EventBody> {
        let id = text_of(block, "id");
        if id.is_empty() {
            return self.fail(
                FailureReason::Adapter,
                "the model started a search without an ID".into(),
            );
        }
        self.querying.insert(index, (id, String::new()));
        self.blocks.insert(index, block.clone());
        Vec::new()
    }

    /// A finished search block: its query goes into the block and the trace. The policy allowed
    /// provider tools before the run, which is what `policy.decided` records.
    fn close_search(&mut self, index: u64, id: String, streamed: &str) -> Vec<EventBody> {
        let block = self.blocks.entry(index).or_insert_with(|| json!({}));
        let query = if streamed.trim().is_empty() {
            block.get("input").cloned()
        } else {
            serde_json::from_str::<Value>(streamed).ok()
        };
        let query = match query {
            Some(object @ Value::Object(_)) => object,
            Some(_) | None => json!({}),
        };
        block["input"] = query.clone();
        let id = ToolCallId::from(clean(&id));
        vec![
            EventBody::ToolRequested(ToolRequested {
                tool_call_id: id.clone(),
                tool: ProviderTool::WebSearch.name().to_owned(),
                action_class: ActionClass::ProviderTools,
                args: query,
            }),
            EventBody::PolicyDecided(PolicyDecided {
                tool_call_id: id.clone(),
                decision: PolicyDecision::Allow,
            }),
            EventBody::ToolStarted(ToolStarted { tool_call_id: id }),
        ]
    }

    /// A search's results, which come whole: titles and links go to the trace; the encrypted
    /// page text stays in the block for Anthropic.
    fn search_result(&mut self, index: u64, block: &Value) -> Vec<EventBody> {
        self.blocks.insert(index, block.clone());
        let id = ToolCallId::from(clean(&text_of(block, "tool_use_id")));
        let (ok, output) = match block.get("content") {
            Some(Value::Array(results)) => {
                if !results.is_empty() {
                    self.read_results = true;
                }
                let shown: Vec<Value> = results
                    .iter()
                    .take(MAX_RESULTS_SHOWN)
                    .map(|r| json!({"title": clean(&text_of(r, "title")), "url": link(&text_of(r, "url"))}))
                    .collect();
                (true, Value::Array(shown))
            }
            Some(error) => (
                false,
                json!({"error": clean(&text_of(error, "error_code"))}),
            ),
            None => (false, json!({"error": "no results came back"})),
        };
        vec![EventBody::ToolResult(ToolResult {
            tool_call_id: id,
            ok,
            output,
        })]
    }

    /// Notes a source the answer cites, once, up to [`MAX_SOURCES`].
    fn cite(&mut self, citation: &Value) {
        let url = link(&text_of(citation, "url"));
        if url.is_empty()
            || self.sources.len() >= MAX_SOURCES
            || self.sources.iter().any(|(_, seen)| *seen == url)
        {
            return;
        }
        self.sources.push((clean(&text_of(citation, "title")), url));
    }

    /// Takes the larger of what's known and what `usage` says: Anthropic's counts are running
    /// totals, so a later, smaller number is never an undo.
    fn note_usage(&mut self, usage: Option<&Value>) {
        let Some(usage) = usage else {
            return;
        };
        let field = |name: &str| usage.get(name).and_then(Value::as_u64);
        let input = [
            "input_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
        ]
        .map(field);
        if input.iter().any(Option::is_some) {
            let total = input
                .iter()
                .flatten()
                .fold(0_u64, |sum, n| sum.saturating_add(*n));
            self.input_tokens = self.input_tokens.max(total);
        }
        if let Some(output) = field("output_tokens") {
            self.output_tokens = self.output_tokens.max(output);
        }
        if let Some(searches) = usage
            .get("server_tool_use")
            .and_then(|s| s.get("web_search_requests"))
            .and_then(Value::as_u64)
        {
            self.searches = self.searches.max(searches);
        }
    }

    /// The answer as the user sees it: the model's text, then the sources it cites.
    fn answer(&self) -> String {
        if self.sources.is_empty() {
            return self.said.clone();
        }
        let mut answer = format!("{}\n\nSources:", self.said);
        for (title, url) in &self.sources {
            if title.is_empty() {
                answer.push_str(&format!("\n- {url}"));
            } else {
                answer.push_str(&format!("\n- {title}: {url}"));
            }
        }
        answer
    }

    fn finish(&mut self) -> Vec<EventBody> {
        self.finished = true;
        let cost = self
            .price
            .input_cost(self.input_tokens)
            .saturating_add(self.price.output_cost(self.output_tokens))
            .saturating_add(self.searches.saturating_mul(WEB_SEARCH_MICRO_USD));
        if !self.open.is_empty() || !self.querying.is_empty() {
            return self.fail(
                FailureReason::Adapter,
                "the response ended in the middle of a tool call".into(),
            );
        }
        self.said = std::mem::take(&mut self.text);
        self.content = std::mem::take(&mut self.blocks).into_values().collect();
        self.paused = self.stop_reason.as_deref() == Some("pause_turn");
        let mut out = vec![
            EventBody::ModelResponse(ModelResponse {
                text: self.answer(),
                finish_reason: self.stop_reason.take(),
            }),
            EventBody::UsageReported(UsageReported {
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                cost_micro_usd: cost,
            }),
        ];
        // With calls to make, or a paused turn, the agent loop goes on.
        if self.calls.is_empty() && !self.paused {
            out.push(EventBody::RunCompleted(RunCompleted { summary: None }));
        }
        out
    }

    fn on_block_start(&mut self, index: u64, block: Option<&Value>) -> Vec<EventBody> {
        let Some(block) = block else {
            return Vec::new();
        };
        let block_type = block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if !block_type.contains("tool") {
            // Text, thinking, redacted thinking, and kinds added later: kept as they came.
            self.blocks.insert(index, block.clone());
            return Vec::new();
        }
        let name = text_of(block, "name");
        if self.tools && block_type == "tool_use" {
            return self.open_call(index, block);
        }
        if self.web_search
            && block_type == "server_tool_use"
            && name == ProviderTool::WebSearch.name()
        {
            return self.open_search(index, block);
        }
        if self.web_search && block_type == "web_search_tool_result" {
            return self.search_result(index, block);
        }
        let tool = if name.is_empty() {
            block_type.to_owned()
        } else {
            name
        };
        self.refuse_tool(&tool)
    }

    fn on_block_stop(&mut self, index: u64) -> Vec<EventBody> {
        if let Some(mut call) = self.open.remove(&index) {
            if call.arguments.trim().is_empty() {
                call.arguments = "{}".into();
            }
            let input = match serde_json::from_str::<Value>(&call.arguments) {
                Ok(object @ Value::Object(_)) => object,
                Ok(_) | Err(_) => json!({}),
            };
            if let Some(block) = self.blocks.get_mut(&index) {
                block["input"] = input;
            }
            self.calls.push(call);
            return Vec::new();
        }
        if let Some((id, streamed)) = self.querying.remove(&index) {
            return self.close_search(index, id, &streamed);
        }
        Vec::new()
    }

    /// Adds streamed input to the call or search at `index`.
    fn on_input(&mut self, index: u64, piece: &str) -> Vec<EventBody> {
        let input = if let Some(call) = self.open.get_mut(&index) {
            &mut call.arguments
        } else if let Some((_, query)) = self.querying.get_mut(&index) {
            query
        } else {
            return Vec::new();
        };
        input.push_str(piece);
        if input.len() > MAX_CALL_ARGUMENT_BYTES {
            let size = input.len() / 1024;
            return self.fail(
                FailureReason::Adapter,
                format!(
                    "the model made a tool call with {size} KB of arguments; the most is {} KB",
                    MAX_CALL_ARGUMENT_BYTES / 1024
                ),
            );
        }
        Vec::new()
    }

    /// Adds `piece` to the string field `key` of the block at `index`, making a text block if
    /// none started.
    fn append(&mut self, index: u64, key: &str, piece: &str) {
        let block = self
            .blocks
            .entry(index)
            .or_insert_with(|| json!({"type": "text", "text": ""}));
        let mut joined = block
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        joined.push_str(piece);
        block[key] = Value::String(joined);
    }

    fn on_delta(&mut self, index: u64, delta: Option<&Value>) -> Vec<EventBody> {
        let Some(delta) = delta else {
            return Vec::new();
        };
        let piece = |key: &str| delta.get(key).and_then(Value::as_str).unwrap_or_default();
        match delta
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "input_json_delta" => self.on_input(index, piece("partial_json")),
            "text_delta" => {
                let text = piece("text").to_owned();
                if text.is_empty() {
                    return Vec::new();
                }
                self.append(index, "text", &text);
                self.text.push_str(&text);
                vec![EventBody::ModelDelta(ModelDelta { text })]
            }
            "citations_delta" => {
                let Some(citation) = delta.get("citation") else {
                    return Vec::new();
                };
                self.cite(citation);
                let block = self
                    .blocks
                    .entry(index)
                    .or_insert_with(|| json!({"type": "text", "text": ""}));
                match block.get_mut("citations") {
                    Some(Value::Array(citations)) => citations.push(citation.clone()),
                    Some(_) | None => block["citations"] = json!([citation]),
                }
                Vec::new()
            }
            // Not the answer, but they go back as they came.
            "thinking_delta" => {
                let thinking = piece("thinking").to_owned();
                self.append(index, "thinking", &thinking);
                Vec::new()
            }
            "signature_delta" => {
                let signature = piece("signature").to_owned();
                self.append(index, "signature", &signature);
                Vec::new()
            }
            // Kinds added later.
            _ => Vec::new(),
        }
    }
}

/// The string field `key` of `value`, or empty.
fn text_of(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// A link for display: no control characters, at most [`MAX_URL_CHARS`].
fn link(url: &str) -> String {
    url.chars()
        .filter(|c| !c.is_control())
        .take(MAX_URL_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

impl StreamDecoder for MessageDecoder {
    fn is_finished(&self) -> bool {
        self.finished
    }

    fn on_event(&mut self, event: &SseEvent) -> Vec<EventBody> {
        if self.finished {
            return Vec::new();
        }
        let Ok(data) = serde_json::from_str::<Value>(&event.data) else {
            return self.fail(
                FailureReason::Adapter,
                "the provider sent an event that isn't JSON".into(),
            );
        };
        let kind = data
            .get("type")
            .and_then(Value::as_str)
            .or(event.event.as_deref())
            .unwrap_or_default();
        let index = data.get("index").and_then(Value::as_u64).unwrap_or(0);
        match kind {
            "message_start" => {
                self.note_usage(data.get("message").and_then(|m| m.get("usage")));
                Vec::new()
            }
            "content_block_start" => self.on_block_start(index, data.get("content_block")),
            "content_block_stop" => self.on_block_stop(index),
            "content_block_delta" => self.on_delta(index, data.get("delta")),
            "message_delta" => {
                if let Some(reason) = data
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                {
                    self.stop_reason = Some(clean(reason));
                }
                self.note_usage(data.get("usage"));
                Vec::new()
            }
            "message_stop" => self.finish(),
            "error" => {
                let message = data
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .map(clean)
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| "the provider reported an error".into());
                self.fail(FailureReason::Provider, message)
            }
            // ping, and event kinds added later.
            _ => Vec::new(),
        }
    }
}
