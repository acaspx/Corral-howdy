import { invoke } from "@tauri-apps/api/core";
import type { AgentEdit, AgentEditorView, ModelView } from "./bindings";

/**
 * Each agent's name, model and monthly cap (M4 item 6). The core checks a save whole against
 * the name rules, the priced models and the caps, keeps it and applies it without a restart;
 * this only shows each agent and sends the form.
 */
export type { AgentEdit, AgentEditorView, ModelView };

export interface AgentEditorApi {
  list(): Promise<AgentEditorView[]>;
  save(agent: string, edit: AgentEdit): Promise<AgentEditorView>;
}

/** The core's commands (apps/desktop/src-tauri/src/agent_editor.rs). */
export const agentEditorApi: AgentEditorApi = {
  list: () => invoke<AgentEditorView[]>("agent_editor"),
  save: (agent, edit) => invoke<AgentEditorView>("save_agent", { agent, edit }),
};

/**
 * A cap typed in dollars ("25", "7.5", "$7.50") as micro-USD, `null` when blank, or
 * `undefined` when it isn't an amount.
 */
export function capFromText(text: string): number | null | undefined {
  const trimmed = text.trim();
  if (trimmed === "") return null;
  const match = /^\$?\s*(\d{1,9})(?:\.(\d{1,2}))?$/.exec(trimmed);
  if (!match) return undefined;
  const cents = (match[2] ?? "").padEnd(2, "0");
  return Number(match[1]) * 1_000_000 + Number(cents) * 10_000;
}

/** A cap in micro-USD as the text the field shows: "" for none, else dollars and cents. */
export function capText(microUsd: number | null): string {
  return microUsd === null ? "" : (microUsd / 1_000_000).toFixed(2);
}
