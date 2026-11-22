import { useEffect, useId, useState } from "react";
import type { RunListItem, RunTrace, Span, SpanKind } from "./bindings";
import { errorText } from "./keys";
import { duration, tracesApi, usd, type TracesApi } from "./tracesApi";

type Filter = "all" | Exclude<SpanKind, "run">;

const FILTERS: { value: Filter; label: string }[] = [
  { value: "all", label: "Everything" },
  { value: "model", label: "Model calls" },
  { value: "tool", label: "Tool calls" },
  { value: "checkpoint", label: "Checkpoints" },
  { value: "mark", label: "Marks" },
];

/** How deep each span sits under the run. */
function depths(spans: Span[]): Map<string, number> {
  const parent = new Map(spans.map((s) => [s.id, s.parent]));
  const depth = new Map<string, number>();
  for (const s of spans) {
    let d = 0;
    let up = s.parent;
    // Bounded by the span count, so a malformed parent loop can't hang the window.
    while (up !== null && up !== undefined && d < spans.length) {
      d += 1;
      up = parent.get(up) ?? null;
    }
    depth.set(s.id, d);
  }
  return depth;
}

/** A percentage, rounded so styles stay short. */
const pct = (n: number) => `${Math.round(n * 1_000) / 1_000}%`;

/**
 * The main window's trace viewer (M4 item 5, TP-TV-13..16): the newest runs, and a run's span tree
 * with a bar for each span on the run's timeline, filtered by kind, with the picked span's details
 * and its events to copy as JSON. Everything shown comes from the core; agent text is shown as text.
 */
export function TraceViewer({ api = tracesApi }: { api?: TracesApi }) {
  const [runs, setRuns] = useState<RunListItem[] | null>(null);
  const [trace, setTrace] = useState<RunTrace | null>(null);
  const [openRun, setOpenRun] = useState<string | null>(null);
  const [picked, setPicked] = useState("run");
  const [filter, setFilter] = useState<Filter>("all");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const headingId = useId();
  const detailsId = useId();
  const filterName = useId();

  async function load() {
    setError(null);
    try {
      setRuns(await api.runs());
    } catch (e: unknown) {
      setError(errorText(e));
    }
  }

  useEffect(() => {
    let live = true;
    api.runs().then(
      (loaded) => live && setRuns(loaded),
      (e: unknown) => live && setError(errorText(e)),
    );
    return () => {
      live = false;
    };
  }, [api]);

  // A task card clicked in the corral's trace tree opens its run here (TP-TRACE-05): the core
  // keeps the run until it's taken, when the viewer loads, comes forward or is nudged.
  useEffect(() => {
    const take = api.takeOpenRun;
    if (!take) return;
    let live = true;
    const ask = () => {
      take().then(
        (runId) => {
          if (live && runId) {
            void load();
            void open(runId);
          }
        },
        (e: unknown) => live && setError(errorText(e)),
      );
    };
    ask();
    const stop = api.onOpenRequest?.(ask);
    return () => {
      live = false;
      stop?.();
    };
    // `load` and `open` only use the api and state setters; the api is the dependency.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [api]);

  async function open(runId: string) {
    setOpenRun(runId);
    setTrace(null);
    setPicked("run");
    setNotice(null);
    setError(null);
    try {
      setTrace(await api.trace(runId));
    } catch (e: unknown) {
      setError(errorText(e));
    }
  }

  async function copy() {
    if (!trace) return;
    setNotice(null);
    setError(null);
    try {
      await api.copy(trace.json);
      setNotice("Copied the run's events as JSON.");
    } catch (e: unknown) {
      setError(`Couldn't copy: ${errorText(e)}`);
    }
  }

  const view = trace?.view;
  const spans = view?.spans ?? [];
  const depth = depths(spans);
  const shown = spans.filter((s) => s.kind === "run" || filter === "all" || s.kind === filter);
  const start = view?.startMs ?? 0;
  const end = view?.endMs ?? Math.max(start, ...spans.map((s) => s.endMs ?? s.startMs));
  const total = Math.max(end - start, 1);
  const detail = spans.find((s) => s.id === picked) ?? spans[0];

  return (
    <section className="key-form traces" aria-labelledby={headingId}>
      <h2 id={headingId}>Traces</h2>
      <div className="trace-actions">
        <button type="button" onClick={() => void load()}>
          Refresh
        </button>
      </div>
      {runs && runs.length === 0 && (
        <p className="key-status">
          No runs yet. Give an agent a task from the corral, and its trace shows here.
        </p>
      )}
      {runs && runs.length > 0 && (
        <ul className="run-list">
          {runs.map((run) => (
            <li key={run.runId}>
              <button
                type="button"
                aria-pressed={openRun === run.runId}
                onClick={() => void open(run.runId)}
              >
                <strong>{run.agentName}</strong> <span className="run-prompt">{run.prompt}</span>{" "}
                <span className={`badge badge-${run.outcome}`}>{run.outcome}</span>{" "}
                <span className="run-cost">{usd(run.costMicroUsd)}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
      {view && (
        <>
          <div className="trace-actions">
            <div role="radiogroup" aria-label="Show" className="trace-filter">
              {FILTERS.map((f) => (
                <label key={f.value}>
                  <input
                    type="radio"
                    name={filterName}
                    checked={filter === f.value}
                    onChange={() => setFilter(f.value)}
                  />
                  {f.label}
                </label>
              ))}
            </div>
            <button type="button" onClick={() => void copy()}>
              Copy as JSON
            </button>
          </div>
          <table className="trace-table" aria-label="Spans">
            <thead>
              <tr>
                <th scope="col">Step</th>
                <th scope="col">Status</th>
                <th scope="col">Time</th>
                <th scope="col">Timeline</th>
              </tr>
            </thead>
            <tbody>
              {shown.map((s) => {
                const d = depth.get(s.id) ?? 0;
                const left = ((s.startMs - start) * 100) / total;
                const width = Math.max((((s.endMs ?? end) - s.startMs) * 100) / total, 0.5);
                return (
                  <tr key={s.id} data-span={s.id} data-depth={d} aria-selected={picked === s.id}>
                    <td>
                      <button
                        type="button"
                        className="span-name"
                        style={{ paddingLeft: `${d * 16}px` }}
                        onClick={() => setPicked(s.id)}
                      >
                        {s.name}
                      </button>
                    </td>
                    <td>
                      <span className={`badge badge-${s.status}`}>{s.status}</span>
                    </td>
                    <td>{s.endMs === null ? "running" : duration(s.endMs - s.startMs)}</td>
                    <td>
                      <div className="trace-track" aria-hidden="true">
                        <div
                          className={`trace-bar trace-${s.kind}`}
                          style={{ left: pct(left), width: pct(width) }}
                        />
                      </div>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          {detail && (
            <section className="trace-details" aria-labelledby={detailsId}>
              <h3 id={detailsId}>Details</h3>
              <p className="span-title">{detail.name}</p>
              <dl>
                <dt>status</dt>
                <dd>{detail.status}</dd>
                {detail.attributes.map((a) => (
                  <div key={a.key} className="span-attribute">
                    <dt>{a.key}</dt>
                    <dd>{a.value}</dd>
                  </div>
                ))}
              </dl>
            </section>
          )}
        </>
      )}
      {notice && (
        <p className="key-status" role="status">
          {notice}
        </p>
      )}
      {error && (
        <p className="key-error" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}
