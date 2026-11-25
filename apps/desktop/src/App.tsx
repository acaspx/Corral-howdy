import { CorralSettings } from "./CorralSettings";
import { corralSettingsApi as defaultCorralApi, type CorralSettingsApi } from "./corralSettingsApi";
import { KeyIntake } from "./KeyIntake";
import { keyApi as defaultKeyApi, type KeyApi } from "./keys";
import { PolicyEditor } from "./PolicyEditor";
import { policyApi as defaultPolicyApi, type PolicyApi } from "./policyApi";
import { ProviderTools } from "./ProviderTools";
import { providerToolsApi as defaultToolsApi, type ProviderToolsApi } from "./providerToolsApi";
import { TraceViewer } from "./TraceViewer";
import { tracesApi as defaultTracesApi, type TracesApi } from "./tracesApi";

/**
 * The main window: Corral's deep-work surface (traces, agent and policy editors, spend, settings).
 * The corral, hover cards and trace tree are native Swift (docs/07-brand-and-corral-surface.md).
 * The API keys, provider tools, the corral's settings, policies and traces are real; the other sections arrive in M4.
 */
const sections = [
  { title: "Agents", body: "Create and edit agents, their providers, tools and budgets." },
  { title: "Spend", body: "What each agent has cost, against its limits." },
] as const;

export function App({
  keyApi = defaultKeyApi,
  toolsApi = defaultToolsApi,
  tracesApi = defaultTracesApi,
  corralApi = defaultCorralApi,
  policyApi = defaultPolicyApi,
}: {
  keyApi?: KeyApi;
  toolsApi?: ProviderToolsApi;
  tracesApi?: TracesApi;
  corralApi?: CorralSettingsApi;
  policyApi?: PolicyApi;
}) {
  return (
    <main className="shell">
      <header>
        <h1>Corral</h1>
        <p className="lede">Your agents live in the corral at the bottom of your screen. This window is for the details.</p>
      </header>
      <ul className="sections" aria-label="Main window sections">
        {sections.map((s) => (
          <li key={s.title}>
            <h2>{s.title}</h2>
            <p>{s.body}</p>
          </li>
        ))}
      </ul>
      <KeyIntake api={keyApi} />
      <ProviderTools api={toolsApi} />
      <PolicyEditor api={policyApi} />
      <CorralSettings api={corralApi} />
      <TraceViewer api={tracesApi} />
      <p className="status" role="status">
        Walking skeleton: connect a key and its agent joins the corral; Grok and Claude take tasks
        today. Right-click the corral to place it, add linked apps, or stop all agents.
      </p>
    </main>
  );
}
