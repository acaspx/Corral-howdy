import { act, fireEvent, render, screen, within } from "@testing-library/react";
import type { CorralSettingsView } from "./bindings";
import { CorralSettings } from "./CorralSettings";
import type { CorralSettingsApi } from "./corralSettingsApi";
import j10Transcript from "../../../native/CorralNative/Fixtures/journeys/J10.json?raw";

// End-to-end journeys, main-window side (Spike S2, docs/04-test-plan.md §6). The Rust core plays
// each journey and records it (apps/desktop/src-tauri/tests/journeys.rs); a step done in the main
// window has a `window`: the user's gesture, the command it calls and the core's answer. This
// plays those steps on the real screen, answering each command with the recorded answer, and
// checks that each gesture asks the core exactly what the transcript says. The corral's steps are
// replayed by CorralJourneyTests.swift, so both halves are checked against one recording.

interface WindowStep {
  command: string;
  args: { ids?: string[] };
  answer: CorralSettingsView;
  gesture: { kind: string; name?: string };
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

describe("journeys in the main window", () => {
  it("J10 Settings → Corral asks which 8 show, then shows the chosen 8", async () => {
    const [open, ...changes] = windowSteps(j10Transcript);
    if (!open) throw new Error("J10 has no main-window steps");
    expect(open.command).toBe("corral_settings");
    const answers = changes.map((change) => change.answer);
    const api: CorralSettingsApi = {
      load: vi.fn(async () => open.answer),
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
    const newcomer = open.answer.agents.find((agent) => !agent.shown);
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
      for (const agent of change.answer.agents) {
        if (agent.shown) expect(box(agent.name), change.say).toBeChecked();
        else expect(box(agent.name), change.say).not.toBeChecked();
      }
      const shown = change.answer.agents.filter((agent) => agent.shown).length;
      expect(section, change.say).toHaveTextContent(`${shown} of 8 shown`);
    }
    expect(answers).toHaveLength(0);
    expect(section).not.toHaveTextContent("Choose which agents show");
  });
});
