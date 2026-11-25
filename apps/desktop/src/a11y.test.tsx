import { composeStories, setProjectAnnotations } from "@storybook/react-vite";
import { cleanup } from "@testing-library/react";
import axe from "axe-core";
import { userEvent } from "storybook/test";
import preview from "../.storybook/preview";
import * as AppStories from "./App.stories";
import css from "./styles.css?raw";

// The main window's accessibility pass (M4 item 8; TP-A11Y-04..06). axe checks every story,
// every state of every component, and the whole window, which is what M4's exit asks for ("axe
// zero serious violations"); this asks for none at all. Colour contrast needs a real renderer,
// so it's checked by eye over the stories (`just storybook`); jsdom can't draw. Tab reaches every
// control in the window, and a focused control always shows a ring of its own.

setProjectAnnotations(preview);

type Runnable = { run: () => Promise<void> };
type StoryModule = Parameters<typeof composeStories>[0];
const modules = import.meta.glob<StoryModule>("./*.stories.tsx", { eager: true });

/** axe's findings for what's on the page now, one line each. */
async function findings(skip: string[] = []): Promise<string[]> {
  const result = await axe.run(document.body, {
    rules: Object.fromEntries(
      ["color-contrast", ...skip].map((rule) => [rule, { enabled: false }]),
    ),
  });
  return result.violations.map(
    (v) => `[${v.impact}] ${v.id}: ${v.help} (${v.nodes.map((n) => n.html.slice(0, 80)).join(" | ")})`,
  );
}

function reset() {
  cleanup();
  document.body.innerHTML = "";
}

const { MainWindow } = composeStories(AppStories) as unknown as { MainWindow: Runnable };

describe("accessibility", () => {
  for (const [file, module] of Object.entries(modules)) {
    const stories = Object.entries(composeStories(module)) as [string, Runnable][];
    for (const [name, Story] of stories) {
      // A component on its own isn't a page: its landmarks are the window's (checked below).
      it(`TP-A11Y-04 ${file.slice(2, -12)}: ${name} has nothing for axe`, async () => {
        try {
          await Story.run();
          expect(await findings(["region"])).toEqual([]);
        } finally {
          reset();
        }
      });
    }
  }

  it("TP-A11Y-04 the whole main window has nothing for axe, landmarks included", async () => {
    try {
      await MainWindow.run();
      expect(document.querySelectorAll("section").length).toBeGreaterThanOrEqual(5);
      expect(await findings()).toEqual([]);
    } finally {
      reset();
    }
  });

  it("TP-A11Y-05 Tab reaches every control in the window, in order", async () => {
    try {
      await MainWindow.run();
      const controls = Array.from(
        document.querySelectorAll<HTMLElement>("button, input, select, textarea, a[href]"),
      ).filter((el) => !el.matches(":disabled"));
      expect(controls.length).toBeGreaterThan(20);
      for (const el of controls) {
        expect(el.tabIndex, el.outerHTML.slice(0, 80)).toBeGreaterThanOrEqual(0);
      }
      const reached: HTMLElement[] = [];
      for (let i = 0; i < controls.length + 5; i++) {
        await userEvent.tab();
        const at = document.activeElement as HTMLElement | null;
        if (!at || at === document.body || reached.includes(at)) break;
        reached.push(at);
      }
      // A radio group is one stop: the arrow keys move within it.
      const stops = controls.filter(
        (el) =>
          !(el instanceof HTMLInputElement && el.type === "radio") ||
          el.checked ||
          !controls.some(
            (o) => o instanceof HTMLInputElement && o.type === "radio" && o.name === el.name && o.checked,
          ),
      );
      expect(reached.map((el) => el.outerHTML.slice(0, 60))).toEqual(
        stops.map((el) => el.outerHTML.slice(0, 60)),
      );
    } finally {
      reset();
    }
  });

  it("TP-A11Y-06 a focused control shows a ring, and only focus draws one", () => {
    expect(css).toMatch(/:focus-visible\s*\{[^}]*outline:\s*2px solid[^}]*\}/);
    const outlines = css
      .replace(/\/\*[\s\S]*?\*\//g, "")
      .split("}")
      .filter((rule) => /outline:\s*(?!none)\S/.test(rule))
      .map((rule) => rule.split("{")[0]?.trim());
    expect(outlines).toEqual([":focus-visible"]);
    expect(css).not.toMatch(/outline:\s*(none|0)\b/);
  });
});
