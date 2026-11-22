import { invoke } from "@tauri-apps/api/core";
import type { RunListItem, RunTrace } from "./bindings";

/**
 * The trace viewer's view of the core (apps/desktop/src-tauri/src/traces.rs): the newest runs, and
 * one run's span tree with its events as JSON. Copying goes through the system clipboard.
 */
export interface TracesApi {
  runs(): Promise<RunListItem[]>;
  trace(runId: string): Promise<RunTrace>;
  copy(text: string): Promise<void>;
  /** The run a task card clicked in the corral asked to open, once (TP-TRACE-05). */
  takeOpenRun?(): Promise<string | null>;
  /** Calls `ask` whenever the window comes forward or the core nudges it; returns a stop. */
  onOpenRequest?(ask: () => void): () => void;
}

/** The DOM event the core dispatches when a click in the corral asks for a run (traces.rs). */
export const OPEN_RUN_EVENT = "corral-open-run";

export const tracesApi: TracesApi = {
  runs: () => invoke<RunListItem[]>("recent_runs"),
  trace: (runId) => invoke<RunTrace>("run_trace", { runId }),
  copy: (text) => navigator.clipboard.writeText(text),
  takeOpenRun: () => invoke<string | null>("take_open_run"),
  onOpenRequest: (ask) => {
    window.addEventListener("focus", ask);
    window.addEventListener(OPEN_RUN_EVENT, ask);
    return () => {
      window.removeEventListener("focus", ask);
      window.removeEventListener(OPEN_RUN_EVENT, ask);
    };
  },
};

/**
 * A cost in micro-USD as the core writes it (corral_core::budget::usd): cents when it's whole
 * cents, otherwise rounded up to a hundredth of a cent, e.g. 1500 → "$0.0015".
 */
export function usd(microUsd: number): string {
  if (microUsd % 10_000 === 0) {
    return `$${(microUsd / 1_000_000).toFixed(2)}`;
  }
  return `$${(Math.ceil(microUsd / 100) / 10_000).toFixed(4)}`;
}

/** A span's length: "850 ms", or "1.2 s" from a second up. */
export function duration(ms: number): string {
  return ms < 1_000 ? `${ms} ms` : `${(ms / 1_000).toFixed(1)} s`;
}
