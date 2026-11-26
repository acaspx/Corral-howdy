/**
 * Fake core APIs for the stories (M4 item 7): each component takes its API as a prop, so a
 * story is one component, a fake that answers the way the core would in that state, and no
 * Tauri. The trace viewer's runs come from real event streams (traces.json, written by
 * src-tauri/tests/story_fixtures.rs from runs played through the supervisor and the store).
 */
import type { AgentEditorApi, AgentEditorView } from "../agentEditorApi";
import type {
  AgentPolicyView,
  Connected,
  CorralSettingsView,
  KeyStatus,
  PolicyClassView,
  Provider,
  ProviderToolsView,
  RunListItem,
  RunTrace,
} from "../bindings";
import type { CorralSettingsApi } from "../corralSettingsApi";
import type { KeyApi } from "../keys";
import type { PolicyApi } from "../policyApi";
import type { ProviderToolsApi } from "../providerToolsApi";
import type { TracesApi } from "../tracesApi";
import recorded from "./traces.json";

/** A promise that never settles: the component stays loading. */
export function never<T>(): Promise<T> {
  return new Promise<T>(() => {});
}

// ---- Keys -----------------------------------------------------------------------------------

const PROVIDERS: Provider[] = ["xai", "openai", "anthropic", "meta"];

export function keyApi(
  connected: Partial<Record<Provider, string>> = {},
  over: Partial<KeyApi> = {},
): KeyApi {
  const status = (provider: Provider): KeyStatus => ({
    provider,
    configured: connected[provider] !== undefined,
    last4: connected[provider] ?? null,
  });
  return {
    status: async (provider) => status(provider),
    identify: async (key) => (key.startsWith("xai-") ? "xai" : key.startsWith("sk-ant-") ? "anthropic" : null),
    connect: async (provider, key): Promise<Connected> => ({
      status: { provider, configured: true, last4: key.slice(-4) },
      checked: true,
    }),
    remove: async (provider) => ({ provider, configured: false, last4: null }),
    ...over,
  };
}

export const allProviders = PROVIDERS;

// ---- Provider tools -------------------------------------------------------------------------

export function toolsView(on = false, over: Partial<ProviderToolsView> = {}): ProviderToolsView {
  return {
    choices: [
      {
        agent: "grok",
        agentName: "Grok",
        tool: "web_search",
        runsOn: "xAI",
        costMicroUsd: 5_000,
        perTurn: 5,
        on,
      },
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
    ...over,
  };
}

export function toolsApi(view: ProviderToolsView = toolsView()): ProviderToolsApi {
  let current = view;
  return {
    list: async () => current,
    set: async (agent, tool, on) => {
      current = {
        ...current,
        choices: current.choices.map((c) =>
          c.agent === agent && c.tool === tool ? { ...c, on } : c,
        ),
      };
      return current;
    },
  };
}

// ---- Corral settings ------------------------------------------------------------------------

export function corralView(over: Partial<CorralSettingsView> = {}): CorralSettingsView {
  return {
    agents: [
      { id: "grok", name: "Grok", shown: true },
      { id: "claude", name: "Claude", shown: true },
      { id: "gpt", name: "GPT", shown: true },
      { id: "muse-spark", name: "Muse Spark", shown: true },
    ],
    keepFixed: false,
    needsChoice: false,
    maxShown: 8,
    ...over,
  };
}

/** Nine linked agents: the corral holds eight, so it asks which to show. */
export function crowdedCorral(): CorralSettingsView {
  const names = ["Grok", "Claude", "GPT", "Muse Spark", "Notes", "Mail", "Calendar", "Files", "Music"];
  return corralView({
    agents: names.map((name, i) => ({
      id: name.toLowerCase().replace(" ", "-"),
      name,
      shown: i < 8,
    })),
    needsChoice: true,
  });
}

export function corralApi(view: CorralSettingsView = corralView()): CorralSettingsApi {
  let current = view;
  return {
    load: async () => current,
    choose: async (ids) => {
      current = {
        ...current,
        needsChoice: false,
        agents: current.agents.map((a) => ({ ...a, shown: ids.includes(a.id) })),
      };
      return current;
    },
    keepFixed: async (keep) => {
      current = { ...current, keepFixed: keep };
      return current;
    },
  };
}

// ---- Policies -------------------------------------------------------------------------------

const cls = (over: Partial<PolicyClassView>): PolicyClassView => ({
  class: "write.local",
  title: "Changing a file in its folder",
  rule: "ask",
  origin: "default",
  default: "ask",
  ceiling: "allow",
  own: null,
  editable: true,
  ...over,
});

export function policyView(over: Partial<AgentPolicyView> = {}): AgentPolicyView {
  return {
    agent: "grok",
    agentName: "Grok",
    classes: [
      cls({ class: "read.local", title: "Reading a file in its folder", rule: "allow", default: "allow" }),
      cls({}),
      cls({ class: "delete.local", title: "Deleting a file in its folder" }),
      cls({ class: "net.read", title: "Reading a web page", rule: "ask" }),
      cls({ class: "comms.send", title: "Sending a message or email", ceiling: "ask" }),
      cls({ class: "money.spend", title: "Spending money", ceiling: "ask" }),
      cls({
        class: "provider_tools",
        title: "A tool on the provider's servers",
        rule: "deny",
        default: "deny",
        editable: false,
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
  };
}

/** Grok with its own rules: writes allowed, sending blocked. */
export function customizedPolicy(): AgentPolicyView {
  const base = policyView();
  return {
    ...base,
    classes: base.classes.map((c) =>
      c.class === "write.local"
        ? { ...c, rule: "allow", origin: "agent", own: "allow" }
        : c.class === "comms.send"
          ? { ...c, rule: "deny", origin: "agent", own: "deny" }
          : c,
    ),
    netAllow: ["*.wikipedia.org", "arxiv.org"],
    onTimeout: "cancelRun",
    loopLimit: 8,
  };
}

export function policyApi(views: AgentPolicyView[] = [policyView()], over: Partial<PolicyApi> = {}): PolicyApi {
  return {
    list: async () => views,
    save: async (agent) => views.find((v) => v.agent === agent) ?? policyView(),
    ...over,
  };
}

// ---- Agents ---------------------------------------------------------------------------------

export function agentView(over: Partial<AgentEditorView> = {}): AgentEditorView {
  return {
    agent: "claude",
    name: "Claude",
    catalogName: "Claude",
    folder: "~/Corral/Claude",
    models: [
      {
        id: "claude-sonnet-5-5",
        name: "Claude Sonnet 5.5",
        price: "$2.00 in and $10.00 out per million tokens",
      },
      {
        id: "claude-opus-5-5",
        name: "Claude Opus 5.5",
        price: "$4.00 in and $20.00 out per million tokens",
      },
      {
        id: "claude-haiku-5-5",
        name: "Claude Haiku 5.5",
        price:
          "$0.10 in and $0.50 out per million tokens; $0.50 and $2.50 from 100,001 prompt tokens",
      },
    ],
    model: "claude-sonnet-5-5",
    monthlyCapMicroUsd: null,
    defaultCapMicroUsd: 25_000_000,
    maxCapMicroUsd: 100_000_000,
    spentMicroUsd: 3_420_000,
    problem: null,
    ...over,
  };
}

export function gptView(): AgentEditorView {
  return agentView({
    agent: "gpt",
    name: "GPT",
    catalogName: "GPT",
    folder: "~/Corral/GPT",
    models: [],
    model: null,
    spentMicroUsd: 0,
  });
}

export function agentApi(
  views: AgentEditorView[] = [agentView(), gptView()],
  over: Partial<AgentEditorApi> = {},
): AgentEditorApi {
  return {
    list: async () => views,
    save: async (agent, edit) => {
      const view = views.find((v) => v.agent === agent) ?? agentView();
      const name = edit.name ?? view.catalogName;
      return {
        ...view,
        name,
        folder: `~/Corral/${name}`,
        model: edit.model,
        monthlyCapMicroUsd: edit.monthlyCapMicroUsd,
      };
    },
    ...over,
  };
}

// ---- Traces ---------------------------------------------------------------------------------

/** Runs played by the FakeAdapter through the supervisor into the store, as the core reads them. */
export const fixtureRuns = recorded.runs as RunListItem[];
const fixtureTraces = recorded.traces as Record<string, RunTrace>;

export function tracesApi(
  runs: RunListItem[] = fixtureRuns,
  { open = null, over = {} }: { open?: string | null; over?: Partial<TracesApi> } = {},
): TracesApi {
  let toOpen = open;
  return {
    runs: async () => runs,
    trace: async (runId) => {
      const trace = fixtureTraces[runId];
      if (!trace) throw `No run ${runId}`;
      return trace;
    },
    copy: async () => {},
    takeOpenRun: async () => {
      const run = toOpen;
      toOpen = null;
      return run;
    },
    ...over,
  };
}
