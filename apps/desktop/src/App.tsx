import { useId } from "react";
import { ChatGptPlan } from "./ChatGptPlan";
import { chatgptApi as defaultPlanApi, type ChatGptApi } from "./chatgptApi";
import { CorralSettings } from "./CorralSettings";
import { corralSettingsApi as defaultCorralApi, type CorralSettingsApi } from "./corralSettingsApi";
import { KeyIntake } from "./KeyIntake";
import { keyApi as defaultKeyApi, type KeyApi } from "./keys";
import { PolicyEditor } from "./PolicyEditor";
import { policyApi as defaultPolicyApi, type PolicyApi } from "./policyApi";
import { AgentEditor } from "./AgentEditor";
import { agentEditorApi as defaultAgentApi, type AgentEditorApi } from "./agentEditorApi";
import { providerToolsApi as defaultToolsApi, type ProviderToolsApi } from "./providerToolsApi";
import { TraceViewer } from "./TraceViewer";
import { tracesApi as defaultTracesApi, type TracesApi } from "./tracesApi";

/**
 * The main window: Corral's deep-work surface (traces, agent and policy editors, spend, settings),
 * built with Glaze (ADR-016). The corral, hover cards and trace tree are native Swift
 * (docs/07-brand-and-corral-surface.md). Spend arrives in M4.
 */
export function App({
  keyApi = defaultKeyApi,
  toolsApi = defaultToolsApi,
  tracesApi = defaultTracesApi,
  corralApi = defaultCorralApi,
  policyApi = defaultPolicyApi,
  agentApi = defaultAgentApi,
  planApi = defaultPlanApi,
}: {
  keyApi?: KeyApi;
  toolsApi?: ProviderToolsApi;
  tracesApi?: TracesApi;
  corralApi?: CorralSettingsApi;
  policyApi?: PolicyApi;
  agentApi?: AgentEditorApi;
  planApi?: ChatGptApi;
}) {
  return (
    <main className="mx-auto flex w-full max-w-3xl flex-col gap-10 px-6 py-10">
      <header className="flex flex-col gap-1">
        <h1 className="type-title">Corral</h1>
        <p className="type-body text-text-2">
          Your agents live in the corral at the bottom of your screen. This window is for the details.
        </p>
      </header>
      <KeyIntake api={keyApi} />
      <ChatGptPlan api={planApi} />
      <AgentEditor api={agentApi} toolsApi={toolsApi} />
      <PolicyEditor api={policyApi} />
      <CorralSettings api={corralApi} />
      <TraceViewer api={tracesApi} />
      <Spend />
      <p className="type-caption text-text-2" role="status">
        Walking skeleton: connect a key and its agent joins the corral; Grok and Claude take tasks
        today. Right-click the corral to place it, add linked apps, or stop all agents.
      </p>
    </main>
  );
}

/** Spend arrives in M4: what each agent has cost, against its limits. */
function Spend() {
  const headingId = useId();
  return (
    <section aria-labelledby={headingId} className="flex flex-col gap-1">
      <h2 id={headingId} className="type-heading">
        Spend
      </h2>
      <p className="type-body-sm text-text-2">What each agent has cost, against its limits.</p>
    </section>
  );
}
