import { act, fireEvent, render, screen, within } from "@testing-library/react";
import type { CorralSettingsView } from "./bindings";
import { CorralSettings } from "./CorralSettings";
import type { CorralSettingsApi } from "./corralSettingsApi";

// TP-USAGE-16: the main window's Corral section. The core decides what shows and keeps the
// order (apps/desktop/src-tauri/src/corral_order.rs); this shows it and asks for changes.

const agent = (id: string, shown: boolean) => ({ id, name: id.toUpperCase(), shown });

const nine = (): CorralSettingsView => ({
  agents: ["a", "b", "c", "d", "e", "f", "g", "h"].map((id) => agent(id, true)).concat(agent("i", false)),
  keepFixed: false,
  needsChoice: true,
  maxShown: 8,
});

function fakeApi(start: CorralSettingsView, overrides: Partial<CorralSettingsApi> = {}): CorralSettingsApi {
  return {
    load: vi.fn(async () => start),
    choose: vi.fn(async (ids: string[]) => ({
      ...start,
      needsChoice: false,
      agents: start.agents.map((a) => ({ ...a, shown: ids.includes(a.id) })),
    })),
    keepFixed: vi.fn(async (keep: boolean) => ({ ...start, keepFixed: keep })),
    ...overrides,
  };
}

async function renderSettings(api: CorralSettingsApi) {
  render(<CorralSettings api={api} />);
  await act(async () => {});
  return screen.getByRole("region", { name: "Corral" });
}

describe("CorralSettings", () => {
  it("TP-USAGE-16 asks which 8 show when a 9th agent arrives, and full, the rest wait", async () => {
    const section = await renderSettings(fakeApi(nine()));
    expect(section).toHaveTextContent("Your corral holds up to 8");
    const boxes = within(section).getAllByRole("checkbox", { name: /^[A-I]$/ });
    expect(boxes).toHaveLength(9);
    const i = within(section).getByRole("checkbox", { name: "I" });
    expect(i).not.toBeChecked();
    expect(i).toHaveAttribute("aria-disabled", "true");
    expect(section).toHaveTextContent("8 of 8 shown");
  });

  it("TP-USAGE-16 a change goes to the core, in the corral's order", async () => {
    const api = fakeApi(nine());
    const section = await renderSettings(api);
    await act(async () => {
      fireEvent.click(within(section).getByRole("checkbox", { name: "H" }));
    });
    expect(api.choose).toHaveBeenCalledWith(["a", "b", "c", "d", "e", "f", "g"]);
    const i = within(section).getByRole("checkbox", { name: "I" });
    expect(i).not.toHaveAttribute("aria-disabled");
    await act(async () => {
      fireEvent.click(i);
    });
    expect(api.choose).toHaveBeenLastCalledWith(["a", "b", "c", "d", "e", "f", "g", "i"]);
  });

  it("TP-USAGE-16 keep order fixed is a switch, and says deploys still count", async () => {
    const api = fakeApi({ ...nine(), needsChoice: false });
    const section = await renderSettings(api);
    expect(section).not.toHaveTextContent("Your corral holds up to 8");
    const fixed = within(section).getByRole("switch", { name: "Keep order fixed" });
    expect(fixed).not.toBeChecked();
    expect(section).toHaveTextContent(/still count/);
    await act(async () => {
      fireEvent.click(fixed);
    });
    expect(api.keepFixed).toHaveBeenCalledWith(true);
    expect(fixed).toBeChecked();
  });

  it("says why when the corral isn't running or a change fails", async () => {
    const section = await renderSettings(
      fakeApi(nine(), {
        load: vi.fn(async () => {
          throw "The corral isn't running yet.";
        }),
      }),
    );
    expect(within(section).getByRole("alert")).toHaveTextContent("The corral isn't running yet.");
    expect(within(section).queryByRole("checkbox")).toBeNull();
  });

  it("a change the core refuses leaves the boxes as they were", async () => {
    const api = fakeApi(nine(), {
      choose: vi.fn(async () => {
        throw "Couldn't save the corral's order: disk full";
      }),
    });
    const section = await renderSettings(api);
    const h = within(section).getByRole("checkbox", { name: "H" });
    await act(async () => {
      fireEvent.click(h);
    });
    expect(h).toBeChecked();
    expect(within(section).getByRole("alert")).toHaveTextContent("disk full");
  });
});
