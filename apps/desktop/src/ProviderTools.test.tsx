import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { ProviderTools } from "./ProviderTools";
import type { ProviderToolChoice, ProviderToolsApi, ProviderToolsView } from "./providerToolsApi";

// TP-PROV-11, 12: the main window's switch for tools that run on a provider's servers. The core
// says what can be switched on and remembers the choice; this only shows it and asks for changes.

const search = (on: boolean): ProviderToolChoice => ({
  agent: "claude",
  agentName: "Claude",
  tool: "web_search",
  runsOn: "Anthropic",
  costMicroUsd: 10_000,
  perTurn: 5,
  on,
});

const view = (on: boolean, problem: string | null = null): ProviderToolsView => ({
  choices: [search(on)],
  problem,
});

function fakeApi(start: ProviderToolsView, overrides: Partial<ProviderToolsApi> = {}): ProviderToolsApi {
  return {
    list: vi.fn(async () => start),
    set: vi.fn(async (_agent: string, _tool: string, on: boolean) => view(on)),
    ...overrides,
  };
}

async function renderTools(api: ProviderToolsApi) {
  render(<ProviderTools api={api} />);
  await act(async () => {});
  return screen.getByRole("region", { name: "Provider tools" });
}

describe("ProviderTools", () => {
  it("TP-PROV-11 shows Claude's search, off, with where it runs and what it costs", async () => {
    const section = await renderTools(fakeApi(view(false)));
    const box = within(section).getByRole("checkbox", { name: "Claude can search the web" });
    expect(box).not.toBeChecked();
    expect(section).toHaveTextContent("Runs on Anthropic's servers");
    expect(section).toHaveTextContent("$0.01 a search, at most 5 a turn");
    expect(section).toHaveTextContent(/untrusted/);
    expect(section).toHaveTextContent(/sends data out asks you first/);
  });

  it("TP-PROV-11 turning it on asks the core and shows what the core says", async () => {
    const api = fakeApi(view(false));
    const section = await renderTools(api);
    const box = within(section).getByRole("checkbox", { name: "Claude can search the web" });
    await act(async () => {
      fireEvent.click(box);
    });
    expect(api.set).toHaveBeenCalledWith("claude", "web_search", true);
    expect(box).toBeChecked();
    await act(async () => {
      fireEvent.click(box);
    });
    expect(api.set).toHaveBeenLastCalledWith("claude", "web_search", false);
    expect(box).not.toBeChecked();
  });

  it("TP-PROV-12 a choice the core couldn't save stays off and says why", async () => {
    const api = fakeApi(view(false), {
      set: vi.fn(async () => {
        throw "Couldn't save your choice: the disk is full.";
      }),
    });
    const section = await renderTools(api);
    const box = within(section).getByRole("checkbox", { name: "Claude can search the web" });
    await act(async () => {
      fireEvent.click(box);
    });
    expect(box).not.toBeChecked();
    expect(within(section).getByRole("alert")).toHaveTextContent(
      "Couldn't save your choice: the disk is full.",
    );
  });

  it("TP-PROV-12 a saved file that couldn't be read is reported", async () => {
    const problem = "Couldn't read your provider tool choices, so they're all off: not JSON.";
    const section = await renderTools(fakeApi(view(false, problem)));
    expect(within(section).getByRole("alert")).toHaveTextContent(problem);
  });

  it("says so when the choices can't be loaded at all", async () => {
    const section = await renderTools(
      fakeApi(view(false), {
        list: vi.fn(async () => {
          throw new Error("no core");
        }),
      }),
    );
    expect(within(section).getByRole("alert")).toHaveTextContent("Couldn't load provider tools: no core");
    expect(within(section).queryByRole("checkbox")).toBeNull();
  });
});
