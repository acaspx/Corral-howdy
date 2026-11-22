import { act, render, screen } from "@testing-library/react";
import type { RunListItem, RunTrace } from "./bindings";
import { TraceViewer } from "./TraceViewer";
import type { TracesApi } from "./tracesApi";

// TP-TRACE-05: clicking a task card in the trace tree opens that run here. The core keeps the
// run asked for; the viewer takes it when it loads, when the window comes forward, or when nudged.

const runs: RunListItem[] = [
  {
    runId: "r2",
    agentName: "Claude",
    prompt: "Tidy my notes",
    queuedMs: 5_000,
    endedMs: null,
    outcome: "running",
    costMicroUsd: 0,
  },
  {
    runId: "r1",
    agentName: "Grok",
    prompt: "Plan the trip",
    queuedMs: 1_000,
    endedMs: 2_000,
    outcome: "completed",
    costMicroUsd: 0,
  },
];

const trace = (runId: string): RunTrace => ({
  view: { runId, startMs: 1_000, endMs: 2_000, spans: [] },
  json: "[]",
});

function openingApi(asked: Array<string | null>) {
  let nudge: (() => void) | null = null;
  const api: TracesApi = {
    runs: vi.fn(async () => runs),
    trace: vi.fn(async (runId: string) => trace(runId)),
    copy: vi.fn(async () => {}),
    takeOpenRun: vi.fn(async () => asked.shift() ?? null),
    onOpenRequest: vi.fn((cb: () => void) => {
      nudge = cb;
      return () => {
        nudge = null;
      };
    }),
  };
  return { api, nudge: () => nudge?.() };
}

describe("TraceViewer, opened from the corral", () => {
  it("TP-TRACE-05 opens the run the corral asked for when it loads", async () => {
    const { api } = openingApi(["r2"]);
    render(<TraceViewer api={api} />);
    await act(async () => {});
    expect(api.trace).toHaveBeenCalledWith("r2");
    expect(screen.getByRole("region", { name: "Traces" })).toBeInTheDocument();
  });

  it("TP-TRACE-05 opens the next one asked for when nudged, and nothing when nothing was asked", async () => {
    const { api, nudge } = openingApi([null, "r1"]);
    render(<TraceViewer api={api} />);
    await act(async () => {});
    expect(api.trace).not.toHaveBeenCalled();
    await act(async () => {
      nudge();
    });
    expect(api.trace).toHaveBeenCalledWith("r1");
  });

  it("a viewer without the corral's requests still works", async () => {
    const api: TracesApi = {
      runs: vi.fn(async () => runs),
      trace: vi.fn(async (runId: string) => trace(runId)),
      copy: vi.fn(async () => {}),
    };
    render(<TraceViewer api={api} />);
    await act(async () => {});
    expect(api.trace).not.toHaveBeenCalled();
  });
});
