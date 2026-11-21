import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import type { RunListItem, RunTrace, Span } from "./bindings";
import { TraceViewer } from "./TraceViewer";
import type { TracesApi } from "./tracesApi";

// TP-TV-13..16: the main window's trace viewer. The core lists runs and projects each run's span
// tree (crates/corral-core/src/trace_view.rs); this only shows them.

const runs: RunListItem[] = [
  {
    runId: "r2",
    agentName: "Claude",
    prompt: "Plan my week",
    queuedMs: 5_000,
    endedMs: null,
    outcome: "running",
    costMicroUsd: 0,
  },
  {
    runId: "r1",
    agentName: "Grok",
    prompt: "Tidy my notes",
    queuedMs: 1_000,
    endedMs: 2_000,
    outcome: "completed",
    costMicroUsd: 1_500,
  },
];

const span = (s: Partial<Span> & Pick<Span, "id" | "kind" | "name" | "startMs">): Span => ({
  parent: "run",
  endMs: s.startMs,
  status: "ok",
  attributes: [],
  ...s,
});

const trace: RunTrace = {
  view: {
    runId: "r1",
    startMs: 1_000,
    endMs: 2_000,
    spans: [
      span({
        id: "run",
        parent: null,
        kind: "run",
        name: "invoke_agent grok",
        startMs: 1_000,
        endMs: 2_000,
        attributes: [
          { key: "prompt", value: "Tidy my notes" },
          { key: "cost", value: "$0.0015" },
        ],
      }),
      span({ id: "model-1", kind: "model", name: "chat grok-4.7", startMs: 1_100, endMs: 1_600 }),
      span({
        id: "tool-c1",
        kind: "tool",
        name: "execute_tool fs.write",
        startMs: 1_600,
        endMs: 1_900,
        status: "denied",
        attributes: [{ key: "decision", value: "ask (high): writes a file" }],
      }),
      span({
        id: "checkpoint-k1",
        parent: "tool-c1",
        kind: "checkpoint",
        name: "checkpoint: Write notes.md",
        startMs: 1_650,
        endMs: 1_850,
        status: "denied",
      }),
      span({ id: "mark-1", kind: "mark", name: "resumed", startMs: 1_950 }),
    ],
  },
  json: '[\n  {"kind": "run.queued"}\n]',
};

function fakeApi(overrides: Partial<TracesApi> = {}): TracesApi {
  return {
    runs: vi.fn(async () => runs),
    trace: vi.fn(async () => trace),
    copy: vi.fn(async () => {}),
    ...overrides,
  };
}

async function renderViewer(api: TracesApi) {
  render(<TraceViewer api={api} />);
  await act(async () => {});
  return screen.getByRole("region", { name: "Traces" });
}

async function openRun(section: HTMLElement, name: RegExp) {
  await act(async () => {
    fireEvent.click(within(section).getByRole("button", { name }));
  });
}

function rowIds(section: HTMLElement): string[] {
  return within(section)
    .getAllByRole("row")
    .map((row) => row.getAttribute("data-span"))
    .filter((id): id is string => id !== null);
}

describe("TraceViewer", () => {
  it("TP-TV-13 lists the newest runs with who ran them and how they went", async () => {
    const section = await renderViewer(fakeApi());
    const buttons = within(section).getAllByRole("button", { name: /Plan my week|Tidy my notes/ });
    expect(buttons.map((b) => b.textContent)).toEqual([
      expect.stringContaining("Claude"),
      expect.stringContaining("Grok"),
    ]);
    expect(buttons[0]).toHaveTextContent("running");
    expect(buttons[1]).toHaveTextContent("completed");
    expect(buttons[1]).toHaveTextContent("$0.0015");
  });

  it("TP-TV-13 says how to get a first trace when there are no runs", async () => {
    const section = await renderViewer(fakeApi({ runs: vi.fn(async () => []) }));
    expect(section).toHaveTextContent("No runs yet");
  });

  it("TP-TV-14 a run opens as its span tree, under the run, with a bar on the timeline", async () => {
    const api = fakeApi();
    const section = await renderViewer(api);
    await openRun(section, /Tidy my notes/);
    expect(api.trace).toHaveBeenCalledWith("r1");
    expect(rowIds(section)).toEqual(["run", "model-1", "tool-c1", "checkpoint-k1", "mark-1"]);

    const row = (id: string) => section.querySelector(`[data-span="${id}"]`) as HTMLElement;
    expect(row("run").getAttribute("data-depth")).toBe("0");
    expect(row("model-1").getAttribute("data-depth")).toBe("1");
    expect(row("checkpoint-k1").getAttribute("data-depth")).toBe("2");
    expect(row("tool-c1")).toHaveTextContent("denied");
    expect(row("model-1")).toHaveTextContent("500 ms");
    expect(row("run")).toHaveTextContent("1.0 s");

    const bar = row("model-1").querySelector(".trace-bar") as HTMLElement;
    expect(bar.style.left).toBe("10%");
    expect(bar.style.width).toBe("50%");
    const mark = row("mark-1").querySelector(".trace-bar") as HTMLElement;
    expect(mark.style.left).toBe("95%");
    expect(parseFloat(mark.style.width)).toBeGreaterThan(0);
  });

  it("TP-TV-14 the run's details show first, and a span's when it's picked", async () => {
    const section = await renderViewer(fakeApi());
    await openRun(section, /Tidy my notes/);
    const details = within(section).getByRole("region", { name: /Details/ });
    expect(details).toHaveTextContent("invoke_agent grok");
    expect(details).toHaveTextContent("prompt");
    expect(details).toHaveTextContent("$0.0015");

    await act(async () => {
      fireEvent.click(within(section).getByRole("button", { name: "execute_tool fs.write" }));
    });
    expect(details).toHaveTextContent("execute_tool fs.write");
    expect(details).toHaveTextContent("ask (high): writes a file");
  });

  it("TP-TV-15 filtering by kind keeps the run and the kind picked", async () => {
    const section = await renderViewer(fakeApi());
    await openRun(section, /Tidy my notes/);
    const show = within(section).getByRole("radiogroup", { name: "Show" });
    for (const [label, ids] of [
      ["Model calls", ["run", "model-1"]],
      ["Tool calls", ["run", "tool-c1"]],
      ["Checkpoints", ["run", "checkpoint-k1"]],
      ["Marks", ["run", "mark-1"]],
      ["Everything", ["run", "model-1", "tool-c1", "checkpoint-k1", "mark-1"]],
    ] as const) {
      await act(async () => {
        fireEvent.click(within(show).getByRole("radio", { name: label }));
      });
      expect(rowIds(section)).toEqual(ids);
    }
  });

  it("TP-TV-16 copies the run's events as JSON", async () => {
    const api = fakeApi();
    const section = await renderViewer(api);
    await openRun(section, /Tidy my notes/);
    await act(async () => {
      fireEvent.click(within(section).getByRole("button", { name: "Copy as JSON" }));
    });
    expect(api.copy).toHaveBeenCalledWith(trace.json);
    expect(within(section).getByRole("status")).toHaveTextContent("Copied the run's events as JSON.");
  });

  it("TP-TV-16 a copy that fails says why", async () => {
    const api = fakeApi({
      copy: vi.fn(async () => {
        throw new Error("not allowed");
      }),
    });
    const section = await renderViewer(api);
    await openRun(section, /Tidy my notes/);
    await act(async () => {
      fireEvent.click(within(section).getByRole("button", { name: "Copy as JSON" }));
    });
    expect(within(section).getByRole("alert")).toHaveTextContent("Couldn't copy: not allowed");
  });

  it("says so when the runs or a trace can't be loaded", async () => {
    let section = await renderViewer(
      fakeApi({
        runs: vi.fn(async () => {
          throw "The event store isn't open yet.";
        }),
      }),
    );
    expect(within(section).getByRole("alert")).toHaveTextContent("The event store isn't open yet.");
    cleanup();

    section = await renderViewer(
      fakeApi({
        trace: vi.fn(async () => {
          throw "No run r1 in the log.";
        }),
      }),
    );
    await openRun(section, /Tidy my notes/);
    expect(within(section).getByRole("alert")).toHaveTextContent("No run r1 in the log.");
  });

  it("shows agent text as text, never as markup", async () => {
    const hostile: RunListItem = {
      runId: "r9",
      agentName: "Grok",
      prompt: '<img src=x onerror="alert(1)">',
      queuedMs: 1_000,
      endedMs: 2_000,
      outcome: "completed",
      costMicroUsd: 0,
    };
    const section = await renderViewer(fakeApi({ runs: vi.fn(async () => [hostile]) }));
    expect(section.querySelector("img")).toBeNull();
    expect(section).toHaveTextContent('<img src=x onerror="alert(1)">');
  });
});
