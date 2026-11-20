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
