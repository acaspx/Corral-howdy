import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { PolicyEditor } from "./PolicyEditor";
import type { AgentPolicyView, PolicyApi, PolicyClassView } from "./policyApi";

// TP-PED-09: the main window's policy editor. The core says what each agent's policy decides and
// why, checks an edit and keeps it; this only shows the policy and sends the form.

const cls = (over: Partial<PolicyClassView>): PolicyClassView => ({
  class: "write.local",
  title: "Saving a file",
  rule: "ask",
  origin: "default",
  default: "ask",
  ceiling: "allow",
  own: null,
  editable: true,
  ...over,
});

const grok = (over: Partial<AgentPolicyView> = {}): AgentPolicyView => ({
  agent: "grok",
  agentName: "Grok",
  classes: [
    cls({ class: "read.local", title: "Reading a file", rule: "allow", default: "allow" }),
    cls({}),
    cls({ class: "money.spend", title: "Spending money", ceiling: "ask" }),
    cls({
      class: "comms.send",
      title: "Sending a message or email",
      rule: "deny",
      origin: "agent",
      own: "deny",
    }),
    cls({
      class: "unknown",
      title: "An action Corral doesn't know",
      rule: "deny",
      origin: "fixed",
      default: "deny",
      ceiling: "deny",
      editable: false,
    }),
  ],
  netAllow: ["*.wikipedia.org"],
  checkpointTimeout: "30m",
  onTimeout: "deny",
  loopLimit: 5,
  problem: null,
  ...over,
});

const claude = (): AgentPolicyView => ({ ...grok(), agent: "claude", agentName: "Claude" });

function fakeApi(overrides: Partial<PolicyApi> = {}): PolicyApi {
  return {
    list: vi.fn(async () => [grok(), claude()]),
    save: vi.fn(async () => grok({ loopLimit: 8 })),
    ...overrides,
  };
}

async function renderEditor(api: PolicyApi) {
  render(<PolicyEditor api={api} />);
  await act(async () => {});
  return screen.getByRole("region", { name: "Policies" });
}

function row(section: HTMLElement, title: string) {
  return within(section).getByRole("row", { name: new RegExp(title) });
}

describe("PolicyEditor", () => {
  it("TP-PED-09 shows each class's decision and where it comes from", async () => {
    const section = await renderEditor(fakeApi());
    expect(within(section).getByRole("combobox", { name: "Agent" })).toHaveValue("grok");
    expect(row(section, "Saving a file")).toHaveTextContent("Ask");
    expect(row(section, "Saving a file")).toHaveTextContent("Default for every agent");
    expect(row(section, "Sending a message or email")).toHaveTextContent("Set for Grok");
    expect(row(section, "An action Corral doesn't know")).toHaveTextContent("Always blocked");
    expect(
      within(row(section, "An action Corral doesn't know")).queryByRole("combobox"),
    ).toBeNull();
  });

  it("TP-PED-09 offers nothing above a class's ceiling", async () => {
    const section = await renderEditor(fakeApi());
    const money = within(row(section, "Spending money")).getByRole("combobox", {
      name: "Spending money",
    });
    const options = within(money).getAllByRole("option").map((o) => o.textContent);
    expect(options).toEqual(["Default (Ask)", "Ask", "Block"]);
    const write = within(row(section, "Saving a file")).getByRole("combobox", {
      name: "Saving a file",
    });
    expect(within(write).getAllByRole("option").map((o) => o.textContent)).toEqual([
      "Default (Ask)",
      "Allow",
      "Ask",
      "Block",
    ]);
  });

  it("TP-PED-09 sends the whole form and shows what the core kept", async () => {
    const api = fakeApi();
    const section = await renderEditor(api);
    fireEvent.change(within(section).getByRole("combobox", { name: "Saving a file" }), {
      target: { value: "allow" },
    });
    fireEvent.change(within(section).getByRole("combobox", { name: "Sending a message or email" }), {
      target: { value: "default" },
    });
    fireEvent.change(within(section).getByRole("textbox", { name: /Allowed web domains/ }), {
      target: { value: "*.wikipedia.org\n\n arxiv.org \n" },
    });
    fireEvent.change(within(section).getByRole("textbox", { name: /Checkpoint wait/ }), {
      target: { value: "10m" },
    });
    fireEvent.change(within(section).getByRole("combobox", { name: /If nobody answers/ }), {
      target: { value: "cancelRun" },
    });
    fireEvent.change(within(section).getByRole("spinbutton", { name: /Loop limit/ }), {
      target: { value: "8" },
    });
    await act(async () => {
      fireEvent.click(within(section).getByRole("button", { name: "Save Grok's policy" }));
    });
    expect(api.save).toHaveBeenCalledWith("grok", {
      rules: [
        { class: "read.local", rule: null },
        { class: "write.local", rule: "allow" },
        { class: "money.spend", rule: null },
        { class: "comms.send", rule: null },
      ],
      netAllow: ["*.wikipedia.org", "arxiv.org"],
      checkpointTimeout: "10m",
      onTimeout: "cancelRun",
      loopLimit: 8,
    });
    expect(within(section).getByRole("status")).toHaveTextContent(
      "Saved. Grok's next task uses it.",
    );
    expect(within(section).getByRole("spinbutton", { name: /Loop limit/ })).toHaveValue(8);
  });

  it("TP-PED-09 a refusal from the core is shown and nothing changes", async () => {
    const api = fakeApi({
      save: vi.fn(async () => {
        throw "Spending money can be at most Ask for any agent";
      }),
    });
    const section = await renderEditor(api);
    await act(async () => {
      fireEvent.click(within(section).getByRole("button", { name: "Save Grok's policy" }));
    });
    expect(within(section).getByRole("alert")).toHaveTextContent("at most Ask");
    expect(within(section).queryByRole("status")).toBeNull();
  });

  it("TP-PED-09 switching agents shows theirs, and a broken policy says so", async () => {
    const api = fakeApi({
      list: vi.fn(async () => [
        grok(),
        { ...claude(), problem: "Claude's saved policy can't be used, so Claude has no tools." },
      ]),
    });
    const section = await renderEditor(api);
    fireEvent.change(within(section).getByRole("combobox", { name: "Agent" }), {
      target: { value: "claude" },
    });
    expect(within(section).getByRole("button", { name: "Save Claude's policy" })).toBeInTheDocument();
    expect(within(section).getByRole("alert")).toHaveTextContent("Claude has no tools");
  });

  it("TP-PED-09 says when the policies can't be loaded", async () => {
    const section = await renderEditor(
      fakeApi({
        list: vi.fn(async () => {
          throw "no app data folder";
        }),
      }),
    );
    expect(within(section).getByRole("alert")).toHaveTextContent(
      "Couldn't load the policies: no app data folder",
    );
  });
});
