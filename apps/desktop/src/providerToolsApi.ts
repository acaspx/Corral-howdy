import { invoke } from "@tauri-apps/api/core";
import type { ProviderTool, ProviderToolChoice, ProviderToolsView } from "./bindings";

/**
 * Tools that run on a provider's servers, such as Claude's web search (M3 item 4). The core says
 * which agents can be given which tools and remembers the choice; the policy decides what an agent
 * with a tool may do. Each is off until turned on here.
 */
export type { ProviderTool, ProviderToolChoice, ProviderToolsView };

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
