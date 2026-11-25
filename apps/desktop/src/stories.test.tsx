import { composeStories, setProjectAnnotations } from "@storybook/react-vite";
import { cleanup } from "@testing-library/react";
import preview from "../.storybook/preview";

// TP-SB-02: every main-window component has stories, and every story renders and plays without
// an error, so the stories stay true as the components change (M4 item 7).

setProjectAnnotations(preview);

type StoryModule = Parameters<typeof composeStories>[0];
/** A composed story: renders it and plays its interactions. */
type Runnable = { run: () => Promise<void> };
const modules = import.meta.glob<StoryModule>("./*.stories.tsx", { eager: true });
const components = Object.keys(import.meta.glob("./*.tsx"))
  .filter((path) => !/\.(test|stories)\.tsx$/.test(path))
  .filter((path) => !["./App.tsx", "./main.tsx"].includes(path));

describe("stories", () => {
  it("TP-SB-02 every component has stories for its states", () => {
    expect(components.length).toBeGreaterThanOrEqual(6);
    for (const component of components) {
      const stories = modules[component.replace(/\.tsx$/, ".stories.tsx")];
      expect(stories, `${component} has stories`).toBeDefined();
      if (stories) {
        expect(Object.keys(composeStories(stories)).length, component).toBeGreaterThanOrEqual(3);
      }
    }
  });

  for (const [file, module] of Object.entries(modules)) {
    const stories = Object.entries(composeStories(module)) as [string, Runnable][];
    for (const [name, Story] of stories) {
      it(`TP-SB-02 ${file.slice(2, -12)}: ${name} renders`, async () => {
        const errors = vi.spyOn(console, "error").mockImplementation(() => {});
        try {
          await Story.run();
          expect(document.body.textContent ?? "").not.toBe("");
          expect(errors).not.toHaveBeenCalled();
        } finally {
          errors.mockRestore();
          cleanup();
          document.body.innerHTML = "";
        }
      });
    }
  }
});
