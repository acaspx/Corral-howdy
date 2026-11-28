import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { AgentEditor } from "./AgentEditor";
import type { AgentEditorApi } from "./agentEditorApi";
import type {
  AgentEdit,
  AgentEditorView,
  Connected,
  CorralSettingsView,
  KeyStatus,
  Provider,
  RunListItem,
  RunTrace,
} from "./bindings";
import { CorralSettings } from "./CorralSettings";
import type { CorralSettingsApi } from "./corralSettingsApi";
import { toolsApi } from "./fixtures/apis";
import { KeyIntake } from "./KeyIntake";
import { TraceViewer } from "./TraceViewer";
import type { TracesApi } from "./tracesApi";
import type { KeyApi } from "./keys";
import j1Transcript from "../../../native/CorralNative/Fixtures/journeys/J1.json?raw";
import j10Transcript from "../../../native/CorralNative/Fixtures/journeys/J10.json?raw";
import j4Transcript from "../../../native/CorralNative/Fixtures/journeys/J4.json?raw";

// End-to-end journeys, main-window side (Spike S2, docs/04-test-plan.md §6). The Rust core plays
// each journey and records it (apps/desktop/src-tauri/tests/journeys.rs); a step done in the main
// window has a `window`: the user's gesture, the command it calls and the core's answer. This
// plays those steps on the real screen, answering each command with the recorded answer, and
// checks that each gesture asks the core exactly what the transcript says. The corral's steps are
// replayed by CorralJourneyTests.swift, so both halves are checked against one recording.

interface WindowStep {
  command: string | null;
  args: {
    ids?: string[];
    providers?: Provider[];
    provider?: Provider;
    agent?: string;
    edit?: AgentEdit;
    runId?: string;
    text?: string;
  };
  answer: unknown;
  gesture: {
    kind: string;
    name?: string;
    agent?: string;
    looksLike?: Provider | null;
    filter?: string;
    spans?: string[];
  };
}

interface Transcript {
  journey: string;
  steps: { say: string; window?: WindowStep }[];
}

/** The journey's main-window steps, in order. */
function windowSteps(raw: string): (WindowStep & { say: string })[] {
  const transcript = JSON.parse(raw) as Transcript;
  return transcript.steps.flatMap((step) => (step.window ? [{ say: step.say, ...step.window }] : []));
}

/** The step that called `command`, in order. */
function called(steps: (WindowStep & { say: string })[], command: string) {
  const step = steps.find((s) => s.command === command);
  if (!step) throw new Error(`the journey never calls ${command}`);
  return step;
}

/** The step whose gesture is `kind`. */
function gestured(steps: (WindowStep & { say: string })[], kind: string) {
  const step = steps.find((s) => s.gesture.kind === kind);
  if (!step) throw new Error(`the journey has no ${kind} step`);
  return step;
}

/** A made-up xAI key, typed into the field; the transcript records only what the core answered. */
const TYPED_KEY = "xai-vitestNotARealKey4Jd8Ws2Pq6Ln"; // gitleaks:allow (made-up test key)

describe("journeys in the main window", () => {
  it("J10 Settings → Corral asks which 8 show, then shows the chosen 8", async () => {
    const [open, ...changes] = windowSteps(j10Transcript);
    if (!open) throw new Error("J10 has no main-window steps");
    expect(open.command).toBe("corral_settings");
    const opened = open.answer as CorralSettingsView;
    const answers = changes.map((change) => change.answer as CorralSettingsView);
    const api: CorralSettingsApi = {
      load: vi.fn(async () => opened),
      choose: vi.fn(async () => {
        const next = answers.shift();
        if (!next) throw new Error("J10 asked the core more than the transcript did");
        return next;
      }),
      keepFixed: vi.fn(async () => {
        throw new Error("J10 doesn't change Keep order fixed");
      }),
    };
    render(<CorralSettings api={api} />);
    await act(async () => {});
    const section = screen.getByRole("region", { name: "Corral" });
    const box = (name: string) => within(section).getByRole("checkbox", { name });

    // A 9th agent arrived: the section asks, the newcomer is unticked, and full, it waits.
    expect(section).toHaveTextContent("Your corral holds up to 8. Choose which agents show.");
    const newcomer = opened.agents.find((agent) => !agent.shown);
    if (!newcomer) throw new Error("J10 opens with every agent shown");
    expect(box(newcomer.name)).not.toBeChecked();
    expect(box(newcomer.name)).toHaveAttribute("aria-disabled", "true");

    for (const change of changes) {
      expect(change.command, change.say).toBe("set_corral_shown");
      const name = change.gesture.name ?? "";
      if (change.gesture.kind === "untick") expect(box(name), change.say).toBeChecked();
      else expect(box(name), change.say).not.toBeChecked();
      await act(async () => {
        fireEvent.click(box(name));
      });
      expect(api.choose, change.say).toHaveBeenLastCalledWith(change.args.ids);
      // The section shows the core's answer.
      const answered = change.answer as CorralSettingsView;
      for (const agent of answered.agents) {
        if (agent.shown) expect(box(agent.name), change.say).toBeChecked();
        else expect(box(agent.name), change.say).not.toBeChecked();
      }
      const shown = answered.agents.filter((agent) => agent.shown).length;
      expect(section, change.say).toHaveTextContent(`${shown} of 8 shown`);
    }
    expect(answers).toHaveLength(0);
    expect(section).not.toHaveTextContent("Choose which agents show");
  });

  it("J1 Settings: an xAI key connects, and the agent editor names Grok Ada", async () => {
    const steps = windowSteps(j1Transcript);

    // First launch: no keys. The key field names the provider the key looks like, and Connect
    // asks the core to check and store it.
    const opening = called(steps, "provider_key_status");
    const statuses = opening.answer as KeyStatus[];
    const connect = called(steps, "connect_provider_key");
    const keyApi: KeyApi = {
      status: vi.fn(async (provider: Provider) => {
        const status = statuses.find((s) => s.provider === provider);
        if (!status) throw new Error(`J1 never asked for ${provider}`);
        return status;
      }),
      identify: vi.fn(async () => connect.gesture.looksLike ?? null),
      connect: vi.fn(async () => connect.answer as Connected),
      remove: vi.fn(async () => {
        throw new Error("J1 removes no key");
      }),
    };
    render(<KeyIntake api={keyApi} />);
    await act(async () => {});
    expect(screen.getByText(/no providers connected yet/i)).toBeInTheDocument();
    await act(async () => {
      fireEvent.change(screen.getByLabelText("API key"), { target: { value: TYPED_KEY } });
    });
    expect(screen.getByText("Looks like an xAI key.")).toBeInTheDocument();
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    });
    expect(keyApi.connect).toHaveBeenCalledWith(connect.args.provider, TYPED_KEY);
    const connected = (connect.answer as Connected).status;
    const providers = screen.getByRole("list", { name: "Connected providers" });
    expect(providers).toHaveTextContent("xAI");
    expect(providers).toHaveTextContent(`ends in ${connected.last4 ?? ""}`);
    expect(document.body.textContent).not.toContain(TYPED_KEY.slice(0, 12));

    // The agent editor: Grok, renamed Ada and saved.
    const listed = called(steps, "agent_editor").answer as AgentEditorView[];
    const save = called(steps, "save_agent");
    const editorApi: AgentEditorApi = {
      list: vi.fn(async () => listed),
      save: vi.fn(async () => save.answer as AgentEditorView),
    };
    render(<AgentEditor api={editorApi} toolsApi={toolsApi()} />);
    await act(async () => {});
    const section = screen.getByRole("region", { name: "Agents" });
    const agent = save.gesture.agent ?? "";
    fireEvent.change(within(section).getByRole("combobox", { name: "Agent" }), {
      target: { value: agent },
    });
    fireEvent.change(within(section).getByRole("textbox", { name: "Name" }), {
      target: { value: save.gesture.name },
    });
    const before = listed.find((view) => view.agent === agent);
    await act(async () => {
      fireEvent.click(within(section).getByRole("button", { name: `Save ${before?.name ?? ""}` }));
    });
    expect(editorApi.save).toHaveBeenCalledWith(save.args.agent, save.args.edit);
    expect(within(section).getByRole("textbox", { name: "Name" })).toHaveValue(
      (save.answer as AgentEditorView).name,
    );
  });

  it("J4 the trace viewer opens the run a task card asked for, shows its tool calls and copies it", async () => {
    const steps = windowSteps(j4Transcript);
    const asked = called(steps, "take_open_run");
    const listed = called(steps, "recent_runs").answer as RunListItem[];
    const opened = called(steps, "run_trace");
    const copied = called(steps, "clipboard");
    let pending = asked.answer as string | null;
    const api: TracesApi = {
      runs: vi.fn(async () => listed),
      trace: vi.fn(async () => opened.answer as RunTrace),
      copy: vi.fn(async () => {}),
      // The run a click in the corral asked for is taken once.
      takeOpenRun: vi.fn(async () => {
        const run = pending;
        pending = null;
        return run;
      }),
      onOpenRequest: () => () => {},
    };
    render(<TraceViewer api={api} />);
    await act(async () => {});
    const section = screen.getByRole("region", { name: "Traces" });
    expect(api.trace).toHaveBeenCalledWith(opened.args.runId);
    const rows = () =>
      within(section)
        .getAllByRole("row")
        .map((row) => row.getAttribute("data-span"))
        .filter((id): id is string => id !== null);
    const spans = (opened.answer as RunTrace).view.spans.map((span) => span.id);
    expect(rows()).toEqual(spans);

    const show = gestured(steps, "show");
    await act(async () => {
      fireEvent.click(
        within(within(section).getByRole("radiogroup", { name: "Show" })).getByRole("radio", {
          name: show.gesture.filter,
        }),
      );
    });
    expect(rows(), show.say).toEqual(show.gesture.spans);

    await act(async () => {
      fireEvent.click(within(section).getByRole("button", { name: "Copy as JSON" }));
    });
    expect(api.copy, copied.say).toHaveBeenCalledWith(copied.args.text);
    expect(within(section).getByRole("status")).toHaveTextContent("Copied the run's events as JSON.");
  });
});

