import { act, render, screen } from "@testing-library/react";
import { GlazeRoot } from "./GlazeRoot";

// TP-GLZ-01: the main window is one Glaze scope (ADR-016), and its theme follows macOS's light
// or dark setting, as the window did before Glaze, which doesn't follow it on its own.

type Listener = (event: { matches: boolean }) => void;

function systemTheme(dark: boolean) {
  const listeners = new Set<Listener>();
  const query = {
    matches: dark,
    addEventListener: (_: "change", listener: Listener) => listeners.add(listener),
    removeEventListener: (_: "change", listener: Listener) => listeners.delete(listener),
  };
  vi.stubGlobal("matchMedia", (q: string) => {
    expect(q).toBe("(prefers-color-scheme: dark)");
    return query;
  });
  return {
    turn(next: boolean) {
      query.matches = next;
      for (const listener of listeners) listener({ matches: next });
    },
    listening: () => listeners.size,
  };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("GlazeRoot", () => {
  it("TP-GLZ-01 puts the window in one Glaze scope", () => {
    systemTheme(false);
    render(
      <GlazeRoot>
        <p>Inside</p>
      </GlazeRoot>,
    );
    const root = screen.getByText("Inside").closest(".glaze");
    expect(root).not.toBeNull();
    expect(root).toHaveClass("min-h-dvh", "bg-page", "text-text-1");
  });

  it("TP-GLZ-01 follows the system's light and dark setting, live", () => {
    const system = systemTheme(true);
    const { unmount } = render(
      <GlazeRoot>
        <p>Inside</p>
      </GlazeRoot>,
    );
    const root = screen.getByText("Inside").closest(".glaze");
    expect(root).toHaveAttribute("data-theme", "dark");
    act(() => system.turn(false));
    expect(root).toHaveAttribute("data-theme", "light");
    act(() => system.turn(true));
    expect(root).toHaveAttribute("data-theme", "dark");
    unmount();
    expect(system.listening()).toBe(0);
  });
});
