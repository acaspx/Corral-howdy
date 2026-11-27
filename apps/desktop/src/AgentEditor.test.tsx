import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { AgentEditor } from "./AgentEditor";
import type { AgentEditorApi, AgentEditorView } from "./agentEditorApi";
import type { ProviderToolsApi, ProviderToolsView } from "./providerToolsApi";

// TP-AED-09: the main window's agent editor. The core checks a save whole, keeps it and applies
// it; this only shows each agent and sends the form.

const claude = (over: Partial<AgentEditorView> = {}): AgentEditorView => ({
  agent: "claude",
  name: "Claude",
  catalogName: "Claude",
  folder: "~/Corral/Claude",
  models: [
    { id: "claude-sonnet-5-5", name: "Claude Sonnet 5.5", price: "$2.00 in and $10.00 out" },
    { id: "claude-haiku-5-5", name: "Claude Haiku 5.5", price: "$0.10 in and $0.50 out" },
  ],
  model: "claude-sonnet-5-5",
  monthlyCapMicroUsd: null,
  defaultCapMicroUsd: 25_000_000,
  maxCapMicroUsd: 100_000_000,
  spentMicroUsd: 1_250_000,
  problem: null,
  ...over,
});

const gpt = (): AgentEditorView => ({
  ...claude(),
  agent: "gpt",
  name: "GPT",
  catalogName: "GPT",
  folder: "~/Corral/GPT",
  models: [],
  model: null,
  spentMicroUsd: null,
});

const tools: ProviderToolsView = {
  choices: [
    {
      agent: "claude",
      agentName: "Claude",
      tool: "web_search",
      runsOn: "Anthropic",
      costMicroUsd: 10_000,
      perTurn: 5,
      on: false,
    },
  ],
  problem: null,
};

function fakeApi(overrides: Partial<AgentEditorApi> = {}): AgentEditorApi {
  return {
    list: vi.fn(async () => [claude(), gpt()]),
    save: vi.fn(async () =>
      claude({
        name: "Ada",
        folder: "~/Corral/Ada",
        model: "claude-haiku-5-5",
        monthlyCapMicroUsd: 7_500_000,
      }),
    ),
    ...overrides,
  };
}

function fakeTools(): ProviderToolsApi {
  return {
    list: vi.fn(async () => tools),
    set: vi.fn(async () => ({
      ...tools,
      choices: tools.choices.map((c) => ({ ...c, on: true })),
    })),
  };
}

async function renderEditor(api: AgentEditorApi, toolsApi = fakeTools()) {
  render(<AgentEditor api={api} toolsApi={toolsApi} />);
  await act(async () => {});
  return screen.getByRole("region", { name: "Agents" });
}

describe("AgentEditor", () => {
  it("TP-AED-09 shows the agent's name, folder, model, cap and spend", async () => {
    const section = await renderEditor(fakeApi());
    expect(within(section).getByRole("combobox", { name: "Agent" })).toHaveValue("claude");
    expect(within(section).getByRole("textbox", { name: "Name" })).toHaveValue("Claude");
    expect(section).toHaveTextContent("Works in ~/Corral/Claude");
    const model = within(section).getByRole("combobox", { name: "Model" });
    expect(model).toHaveValue("claude-sonnet-5-5");
    expect(within(model).getAllByRole("option").map((o) => o.textContent)).toEqual([
      "Claude Sonnet 5.5",
      "Claude Haiku 5.5",
    ]);
    expect(section).toHaveTextContent("$2.00 in and $10.00 out");
    fireEvent.change(model, { target: { value: "claude-haiku-5-5" } });
    expect(section).toHaveTextContent("$0.10 in and $0.50 out");
    expect(section).not.toHaveTextContent("$2.00 in and $10.00 out");
    expect(within(section).getByRole("textbox", { name: /Monthly cap/ })).toHaveValue("");
    expect(section).toHaveTextContent("Empty is the default, $25.00. At most $100.00.");
    expect(section).toHaveTextContent("Spent $1.25 this month.");
  });

  it("TP-AED-09 sends the form and shows what the core kept", async () => {
    const api = fakeApi();
    const section = await renderEditor(api);
    fireEvent.change(within(section).getByRole("textbox", { name: "Name" }), {
      target: { value: " Ada " },
    });
    fireEvent.change(within(section).getByRole("combobox", { name: "Model" }), {
      target: { value: "claude-haiku-5-5" },
    });
    fireEvent.change(within(section).getByRole("textbox", { name: /Monthly cap/ }), {
      target: { value: "$7.50" },
    });
    await act(async () => {
      fireEvent.click(within(section).getByRole("button", { name: "Save Claude" }));
    });
    expect(api.save).toHaveBeenCalledWith("claude", {
      name: "Ada",
      model: "claude-haiku-5-5",
      monthlyCapMicroUsd: 7_500_000,
    });
    expect(within(section).getByRole("status")).toHaveTextContent(
      "Saved. Ada's next task uses it.",
    );
    expect(section).toHaveTextContent("Works in ~/Corral/Ada");
    expect(within(section).getByRole("textbox", { name: /Monthly cap/ })).toHaveValue("7.50");
  });

  it("TP-AED-09 an empty name and cap send null", async () => {
    const api = fakeApi();
    const section = await renderEditor(api);
    fireEvent.change(within(section).getByRole("textbox", { name: "Name" }), {
      target: { value: "  " },
    });
    await act(async () => {
      fireEvent.click(within(section).getByRole("button", { name: "Save Claude" }));
    });
    expect(api.save).toHaveBeenCalledWith("claude", {
      name: null,
      model: "claude-sonnet-5-5",
      monthlyCapMicroUsd: null,
    });
  });

  it("TP-AED-09 a cap that isn't an amount isn't sent", async () => {
    const api = fakeApi();
    const section = await renderEditor(api);
    fireEvent.change(within(section).getByRole("textbox", { name: /Monthly cap/ }), {
      target: { value: "lots" },
    });
    await act(async () => {
      fireEvent.click(within(section).getByRole("button", { name: "Save Claude" }));
    });
    expect(api.save).not.toHaveBeenCalled();
    expect(within(section).getByRole("alert")).toHaveTextContent(
      "The monthly cap has to be an amount in dollars, like 25 or 7.50.",
    );
  });

  it("TP-AED-09 a refusal from the core is shown and nothing changes", async () => {
    const api = fakeApi({
      save: vi.fn(async () => {
        throw "~/Corral/Iris already exists.";
      }),
    });
    const section = await renderEditor(api);
    await act(async () => {
      fireEvent.click(within(section).getByRole("button", { name: "Save Claude" }));
    });
    expect(within(section).getByRole("alert")).toHaveTextContent("~/Corral/Iris already exists.");
    expect(within(section).queryByRole("status")).toBeNull();
  });

  it("TP-AED-09 the agent's provider tools are switched here", async () => {
    const toolsApi = fakeTools();
    const section = await renderEditor(fakeApi(), toolsApi);
    const search = within(section).getByRole("switch", { name: "Claude can search the web" });
    await act(async () => {
      fireEvent.click(search);
    });
    expect(toolsApi.set).toHaveBeenCalledWith("claude", "web_search", true);
  });

  it("TP-AED-09 an agent Corral can't run yet says so, and a problem is shown", async () => {
    const api = fakeApi({
      list: vi.fn(async () => [claude({ problem: "Claude can't spend until saved." }), gpt()]),
    });
    const section = await renderEditor(api);
    expect(within(section).getByRole("alert")).toHaveTextContent("Claude can't spend");
    await act(async () => {
      fireEvent.change(within(section).getByRole("combobox", { name: "Agent" }), {
        target: { value: "gpt" },
      });
    });
    expect(within(section).queryByRole("combobox", { name: "Model" })).toBeNull();
    expect(section).toHaveTextContent("Corral can't run GPT yet.");
    expect(section).toHaveTextContent("No provider tools for GPT yet.");
    expect(within(section).getByRole("button", { name: "Save GPT" })).toBeInTheDocument();
  });

  it("TP-AED-09 says when the agents can't be loaded", async () => {
    const section = await renderEditor(
      fakeApi({
        list: vi.fn(async () => {
          throw "no app data folder";
        }),
      }),
    );
    expect(within(section).getByRole("alert")).toHaveTextContent(
      "Couldn't load the agents: no app data folder",
    );
  });

  // TP-SUB-08: GPT's model on the ChatGPT plan. The core lists the plan's models and keeps the
  // choice; GPT spends no dollars, so its cap and spend aren't shown.
  const onPlan = (over: Partial<AgentEditorView> = {}): AgentEditorView => ({
    ...gpt(),
    models: [
      { id: "gpt-alpha", name: "GPT Alpha", price: "Uses your ChatGPT plan" },
      { id: "gpt-beta", name: "GPT Beta", price: "Uses your ChatGPT plan" },
    ],
    model: "gpt-alpha",
    chatgptPlan: { state: "signedIn" },
    ...over,
  });

  it("TP-SUB-08 GPT picks a model from the plan, with no dollar cap", async () => {
    const api = fakeApi({
      list: vi.fn(async () => [onPlan()]),
      save: vi.fn(async () => onPlan({ model: "gpt-beta" })),
    });
    const section = await renderEditor(api);
    const model = within(section).getByRole("combobox", { name: "Model" });
    expect(within(model).getAllByRole("option").map((o) => o.textContent)).toEqual([
      "GPT Alpha",
      "GPT Beta",
    ]);
    expect(section).toHaveTextContent("Uses your ChatGPT plan");
    expect(section).toHaveTextContent("No dollar cap: GPT's tasks count against your plan's usage.");
    expect(within(section).queryByRole("textbox", { name: /Monthly cap/ })).toBeNull();
    expect(section).not.toHaveTextContent("this month");
    fireEvent.change(model, { target: { value: "gpt-beta" } });
    await act(async () => {
      fireEvent.click(within(section).getByRole("button", { name: "Save GPT" }));
    });
    expect(api.save).toHaveBeenCalledWith("gpt", {
      name: "GPT",
      model: "gpt-beta",
      monthlyCapMicroUsd: null,
    });
    expect(within(section).getByRole("combobox", { name: "Model" })).toHaveValue("gpt-beta");
  });

  it("TP-SUB-08 signed out or unreachable, GPT says what to do", async () => {
    const section = await renderEditor(
      fakeApi({ list: vi.fn(async () => [onPlan({ models: [], model: null, chatgptPlan: { state: "signedOut" } })]) }),
    );
    expect(section).toHaveTextContent(
      "GPT runs on your ChatGPT plan. Continue with ChatGPT above to choose its model.",
    );
    expect(section).not.toHaveTextContent("Corral can't run GPT yet.");
    expect(within(section).queryByRole("combobox", { name: "Model" })).toBeNull();
  });

  it("TP-SUB-08 when the plan can't be asked, says why", async () => {
    const section = await renderEditor(
      fakeApi({
        list: vi.fn(async () => [
          onPlan({
            models: [],
            model: null,
            chatgptPlan: { state: "unavailable", message: "Couldn't reach ChatGPT." },
          }),
        ]),
      }),
    );
    expect(section).toHaveTextContent("Couldn't reach ChatGPT.");
  });
});

