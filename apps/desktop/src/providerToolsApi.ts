import { invoke } from "@tauri-apps/api/core";

/**
 * Tools that run on a provider's servers, such as Claude's web search (M3 item 4). The core says
 * which agents can be given which tools and remembers the choice; the policy decides what an agent
 * with a tool may do. Each is off until turned on here.
 */
export type ProviderTool = "web_search";

export interface ProviderToolChoice {
  agent: string;
  agentName: string;
  tool: ProviderTool;
  /** Whose servers it runs on, e.g. "Anthropic". */
  runsOn: string;
  /** What one use costs, in millionths of a US dollar. */
  costMicroUsd: number;
  /** The most uses in one turn. */
  perTurn: number;
  on: boolean;
}

export interface ProviderToolsView {
  choices: ProviderToolChoice[];
  /** Why every tool is off despite the saved choices, when they couldn't be read. */
  problem: string | null;
}

export interface ProviderToolsApi {
  list(): Promise<ProviderToolsView>;
  set(agent: string, tool: ProviderTool, on: boolean): Promise<ProviderToolsView>;
}

/** The core's commands (apps/desktop/src-tauri/src/provider_tools.rs). */
export const providerToolsApi: ProviderToolsApi = {
  list: () => invoke<ProviderToolsView>("provider_tools"),
  set: (agent, tool, on) => invoke<ProviderToolsView>("set_provider_tool", { agent, tool, on }),
};

/** A cost in micro-USD as dollars, e.g. 10000 → "$0.01". */
export function dollars(microUsd: number): string {
  return `$${(microUsd / 1_000_000).toFixed(2)}`;
}
