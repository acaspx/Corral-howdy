/**
 * The main window: Corral's deep-work surface (traces, agent and policy editors, spend, settings).
 * The corral itself and the activity cards are native Swift (docs/07-brand-and-corral-surface.md).
 * M0 skeleton: real screens arrive in milestone M4.
 */
const sections = [
  { title: "Traces", body: "Every model call, tool call and checkpoint for each run, in order." },
  { title: "Agents", body: "Create and edit agents, their providers, tools and budgets." },
  { title: "Policies", body: "What each agent may do on its own, and what stops for your approval." },
  { title: "Spend", body: "What each agent has cost, against its limits." },
] as const;

export function App() {
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
      <p className="status" role="status">
        Skeleton build (milestone M0). Nothing is wired up yet.
      </p>
    </main>
  );
}
