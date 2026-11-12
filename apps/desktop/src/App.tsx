import { ProviderKeyForm } from "./ProviderKeyForm";
import { keyApi as defaultKeyApi, type KeyApi } from "./keys";

/**
 * The main window: Corral's deep-work surface (traces, agent and policy editors, spend, settings).
 * The corral, hover cards and trace tree are native Swift (docs/07-brand-and-corral-surface.md).
 * Walking skeleton: the xAI key form (SK-3) is the first real screen; the rest arrive in M4.
 */
const sections = [
  { title: "Traces", body: "Every model call, tool call and checkpoint for each run, in order." },
  { title: "Agents", body: "Create and edit agents, their providers, tools and budgets." },
  { title: "Policies", body: "What each agent may do on its own, and what stops for your approval." },
  { title: "Spend", body: "What each agent has cost, against its limits." },
] as const;

export function App({ keyApi = defaultKeyApi }: { keyApi?: KeyApi }) {
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
      <ProviderKeyForm provider="xai" name="xAI" api={keyApi} />
      <p className="status" role="status">
        Walking skeleton: add your xAI key now; your first agent arrives next.
      </p>
    </main>
  );
}
