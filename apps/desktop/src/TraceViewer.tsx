import { useEffect, useId, useState, type CSSProperties } from "react";
import { Alert, AlertDescription } from "@/components/glaze/ui/alert";
import { Button } from "@/components/glaze/ui/button";
import {
  DataList,
  DataListItem,
  DataListLabel,
  DataListValue,
} from "@/components/glaze/ui/data-list";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
} from "@/components/glaze/ui/empty-state";
import {
  SegmentedControl,
  SegmentedControlItem,
} from "@/components/glaze/ui/segmented-control";
import { Status } from "@/components/glaze/ui/status";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/glaze/ui/table";
import { cn } from "@/lib/glaze/utils";
import type {
  RunListItem,
  RunListOutcome,
  RunTrace,
  Span,
  SpanKind,
  SpanStatus,
} from "./bindings";
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

type Tone = "running" | "ok" | "fail" | "idle";

/** How each outcome reads; the word stays the core's. */
const OUTCOME_TONE: Record<RunListOutcome, Tone> = {
  running: "running",
  completed: "ok",
  failed: "fail",
  cancelled: "idle",
};

const SPAN_TONE: Record<SpanStatus, Tone> = {
  running: "running",
  ok: "ok",
  error: "fail",
  denied: "fail",
  cancelled: "idle",
};

const isFilter = (value: string): value is Filter =>
  FILTERS.some((f) => f.value === value);

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

/** Custom properties for a class to read, such as start-(--start). */
const vars = (values: Record<`--${string}`, string>) => values as CSSProperties;

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
  const shown = spans.filter(
    (s) => s.kind === "run" || filter === "all" || s.kind === filter,
  );
  const start = view?.startMs ?? 0;
  const end =
    view?.endMs ?? Math.max(start, ...spans.map((s) => s.endMs ?? s.startMs));
  const total = Math.max(end - start, 1);
  const detail = spans.find((s) => s.id === picked) ?? spans[0];

  return (
    <section aria-labelledby={headingId} className="flex flex-col gap-3">
      <div className="flex items-center gap-3">
        <h2 id={headingId} className="type-heading">
          Traces
        </h2>
        <Button
          variant="secondary"
          size="sm"
          className="ms-auto"
          onClick={() => void load()}
        >
          Refresh
        </Button>
      </div>
      {runs && runs.length === 0 && (
        <Empty>
          <EmptyHeader>
            <EmptyTitle>No runs yet</EmptyTitle>
            <EmptyDescription>
              Give an agent a task from the corral, and its trace shows here.
            </EmptyDescription>
          </EmptyHeader>
        </Empty>
      )}
      {runs && runs.length > 0 && (
        <Table aria-label="Runs" maxHeight={240} stickyHeader>
          <TableHeader>
            <TableRow>
              <TableHead>Run</TableHead>
              <TableHead>Outcome</TableHead>
              <TableHead numeric>Cost</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {runs.map((run) => (
              <TableRow key={run.runId} selected={openRun === run.runId}>
                <TableCell className="w-full max-w-0 ps-1">
                  <Button
                    variant="ghost"
                    size="sm"
                    aria-pressed={openRun === run.runId}
                    onClick={() => void open(run.runId)}
                  >
                    <span>{run.agentName}</span>
                    <span className="truncate text-text-2">{run.prompt}</span>
                  </Button>
                </TableCell>
                <TableCell>
                  <Status status={OUTCOME_TONE[run.outcome]} size="sm">
                    {run.outcome}
                  </Status>
                </TableCell>
                <TableCell numeric>{usd(run.costMicroUsd)}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      {view && (
        <>
          <div className="flex flex-wrap items-center gap-3">
            <SegmentedControl
              size="sm"
              aria-label="Show"
              value={filter}
              onValueChange={(value) => isFilter(value) && setFilter(value)}
            >
              {FILTERS.map((f) => (
                <SegmentedControlItem key={f.value} value={f.value}>
                  {f.label}
                </SegmentedControlItem>
              ))}
            </SegmentedControl>
            <Button
              variant="secondary"
              size="sm"
              className="ms-auto"
              onClick={() => void copy()}
            >
              Copy as JSON
            </Button>
          </div>
          <Table aria-label="Spans">
            <TableHeader>
              <TableRow>
                <TableHead>Step</TableHead>
                <TableHead>Status</TableHead>
                <TableHead numeric>Time</TableHead>
                <TableHead className="w-full">Timeline</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {shown.map((s) => {
                const d = depth.get(s.id) ?? 0;
                const left = ((s.startMs - start) * 100) / total;
                const width = Math.max(
                  (((s.endMs ?? end) - s.startMs) * 100) / total,
                  0.5,
                );
                const trouble = SPAN_TONE[s.status] === "fail";
                return (
                  <TableRow
                    key={s.id}
                    data-span={s.id}
                    data-depth={d}
                    selected={picked === s.id}
                  >
                    <TableCell
                      className="ps-(--indent)"
                      style={vars({
                        "--indent": `calc(var(--space-1) + ${d} * var(--space-4))`,
                      })}
                    >
                      <Button
                        variant="ghost"
                        size="sm"
                        aria-pressed={picked === s.id}
                        onClick={() => setPicked(s.id)}
                      >
                        {s.name}
                      </Button>
                    </TableCell>
                    <TableCell>
                      <Status status={SPAN_TONE[s.status]} size="sm">
                        {s.status}
                      </Status>
                    </TableCell>
                    <TableCell numeric>
                      {s.endMs === null
                        ? "running"
                        : duration(s.endMs - s.startMs)}
                    </TableCell>
                    <TableCell>
                      <div aria-hidden="true" className="relative h-3 min-w-40">
                        <div
                          data-slot="trace-bar"
                          data-kind={s.kind}
                          className={cn(
                            "absolute inset-y-0 start-(--start) w-(--width) min-w-1 rounded-pill forced-colors:bg-[CanvasText]",
                            trouble
                              ? "bg-fail"
                              : picked === s.id
                                ? "bg-ink"
                                : "bg-line-strong",
                          )}
                          style={vars({
                            "--start": pct(left),
                            "--width": pct(width),
                          })}
                        />
                      </div>
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
          {detail && (
            <section
              aria-labelledby={detailsId}
              className="flex flex-col gap-1"
            >
              <h3 id={detailsId} className="type-heading-sm">
                Details
              </h3>
              <p className="type-label">{detail.name}</p>
              <DataList>
                <DataListItem>
                  <DataListLabel>status</DataListLabel>
                  <DataListValue>{detail.status}</DataListValue>
                </DataListItem>
                {detail.attributes.map((a) => (
                  <DataListItem key={a.key}>
                    <DataListLabel>{a.key}</DataListLabel>
                    <DataListValue className="whitespace-pre-wrap">
                      {a.value}
                    </DataListValue>
                  </DataListItem>
                ))}
              </DataList>
            </section>
          )}
        </>
      )}
      {notice && (
        <Alert status="ok">
          <AlertDescription>{notice}</AlertDescription>
        </Alert>
      )}
      {error && (
        <Alert status="fail">
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}
    </section>
  );
}
