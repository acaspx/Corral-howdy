import { invoke } from "@tauri-apps/api/core";
import type { CorralAgentChoice, CorralSettingsView } from "./bindings";

/**
 * The corral's settings (M4 item 2): which agents it shows, at most 8, and whether its order
 * stays fixed. The core decides and saves (apps/desktop/src-tauri/src/corral_order.rs); each call
 * answers with the settings after it.
 */
export type { CorralAgentChoice, CorralSettingsView };

export interface CorralSettingsApi {
  load(): Promise<CorralSettingsView>;
  /** Shows these agents, in the corral's order. */
  choose(ids: string[]): Promise<CorralSettingsView>;
  keepFixed(keep: boolean): Promise<CorralSettingsView>;
}

/** The core's commands (apps/desktop/src-tauri/src/corral_settings.rs). */
export const corralSettingsApi: CorralSettingsApi = {
  load: () => invoke<CorralSettingsView>("corral_settings"),
  choose: (ids) => invoke<CorralSettingsView>("set_corral_shown", { ids }),
  keepFixed: (keep) => invoke<CorralSettingsView>("set_keep_order_fixed", { keep }),
};
