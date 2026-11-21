// Generated from the Rust types by apps/desktop/src-tauri/src/bindings.rs (TP-IPC-06).
// Don't edit: change the Rust type, then run
//   CORRAL_WRITE_BINDINGS=1 cargo test -p corral-desktop --test bindings

export type Provider = "xai" | "openai" | "anthropic" | "meta";

export type KeyStatus = {
/**
 * The provider.
 */
provider: Provider,
/**
 * True when a key is stored.
 */
configured: boolean,
/**
 * Its last 4 characters, when there is one.
 */
last4: string | null, };

export type Connected = {
/**
 * The provider's key status.
 */
status: KeyStatus,
/**
 * Whether the provider accepted the key (`false`: there's no check for it yet).
 */
checked: boolean, };

export type ProviderTool = "web_search";

export type ProviderToolChoice = {
/**
 * The agent's ID, e.g. `claude`.
 */
agent: string,
/**
 * Its name, e.g. `Claude`.
 */
agentName: string,
/**
 * The tool.
 */
tool: ProviderTool,
/**
 * Whose servers it runs on, e.g. `Anthropic`.
 */
runsOn: string,
/**
 * What one use costs, in micro-USD. A JSON number: far below 2^53.
 */
costMicroUsd: number,
/**
 * The most uses in one turn.
 */
perTurn: number,
/**
 * Whether it's on.
 */
on: boolean, };

export type ProviderToolsView = {
/**
 * Each tool an agent can be given, in the catalog's order.
 */
choices: Array<ProviderToolChoice>,
/**
 * Why every tool is off despite the file, in plain words.
 */
problem: string | null, };

export type RunListItem = {
/**
 * The run.
 */
runId: string,
/**
 * Its agent's name, e.g. "Grok", or its ID when Corral doesn't know it.
 */
agentName: string,
/**
 * The prompt, cleaned and capped.
 */
prompt: string,
/**
 * When it was queued, in ms since the epoch.
 */
queuedMs: number,
/**
 * When it ended, if it has.
 */
endedMs: number | null,
/**
 * How it ended, or that it hasn't.
 */
outcome: RunListOutcome,
/**
 * What it cost, in micro-USD.
 */
costMicroUsd: number, };

export type RunListOutcome = "running" | "completed" | "failed" | "cancelled";

export type RunTrace = {
/**
 * The span tree.
 */
view: TraceView,
/**
 * The run's events, as the log holds them, indented.
 */
json: string, };

export type CorralSettingsView = {
/**
 * Every agent.
 */
agents: Array<CorralAgentChoice>,
/**
 * "Keep order fixed" is on.
 */
keepFixed: boolean,
/**
 * An agent was added to a full corral and the user hasn't chosen which 8 show.
 */
needsChoice: boolean,
/**
 * The most agents the corral shows.
 */
maxShown: number, };

export type CorralAgentChoice = {
/**
 * The agent.
 */
id: string,
/**
 * Its name, e.g. "Grok".
 */
name: string,
/**
 * Whether the corral shows it.
 */
shown: boolean, };

export type TraceView = {
/**
 * The run.
 */
runId: string,
/**
 * When its first event was logged, in ms since the epoch.
 */
startMs: number,
/**
 * When it ended, or none while it's going.
 */
endMs: number | null,
/**
 * Its spans.
 */
spans: Array<Span>, };

export type Span = {
/**
 * Unique in the run: `run`, `model-1`, `tool-<call>`, `checkpoint-<id>`, `mark-<n>`.
 */
id: string,
/**
 * The span it's under, or none for the run.
 */
parent: string | null,
/**
 * What it is.
 */
kind: SpanKind,
/**
 * Its name, e.g. "chat claude-sonnet-5-5".
 */
name: string,
/**
 * When it began, in ms since the epoch.
 */
startMs: number,
/**
 * When it ended, or none while it's open.
 */
endMs: number | null,
/**
 * How it went.
 */
status: SpanStatus,
/**
 * What the side panel shows, in order.
 */
attributes: Array<Attribute>, };

export type Attribute = {
/**
 * The label, e.g. "input tokens".
 */
key: string,
/**
 * The value, as text.
 */
value: string, };

export type SpanKind = "run" | "model" | "tool" | "checkpoint" | "mark";

export type SpanStatus = "running" | "ok" | "error" | "denied" | "cancelled";
