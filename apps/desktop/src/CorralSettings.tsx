import { useEffect, useId, useState } from "react";
import {
  corralSettingsApi,
  type CorralAgentChoice,
  type CorralSettingsApi,
  type CorralSettingsView,
} from "./corralSettingsApi";
import { errorText } from "./keys";

/**
 * The main window's Corral section (TP-USAGE-16): a box per agent for whether the corral shows it,
 * and "Keep order fixed". The core decides and saves; this shows its answer. Once 8 show, the
 * rest wait until one is unticked.
 */
export function CorralSettings({ api = corralSettingsApi }: { api?: CorralSettingsApi }) {
  const [view, setView] = useState<CorralSettingsView | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const headingId = useId();
  const hintId = useId();

  useEffect(() => {
    let live = true;
    api.load().then(
      (loaded) => live && setView(loaded),
      (e: unknown) => live && setError(errorText(e)),
    );
    return () => {
      live = false;
    };
  }, [api]);

  async function change(ask: () => Promise<CorralSettingsView>) {
    setBusy(true);
    setError(null);
    try {
      setView(await ask());
    } catch (e: unknown) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  }

  function toggle(current: CorralSettingsView, agent: CorralAgentChoice) {
    const ids = current.agents
      .filter((a) => (a.id === agent.id ? !a.shown : a.shown))
      .map((a) => a.id);
    void change(() => api.choose(ids));
  }

  const shownCount = view?.agents.filter((a) => a.shown).length ?? 0;
  const full = view !== null && shownCount >= view.maxShown;
  return (
    <section className="key-form" aria-labelledby={headingId}>
      <h2 id={headingId}>Corral</h2>
      <p className="key-status">
        The corral shows up to {view?.maxShown ?? 8} of your agents, most used first. Agents you
        don't show stay connected.
      </p>
      {view?.needsChoice && (
        <p className="key-status" role="status">
          Your corral holds up to {view.maxShown}. Choose which agents show.
        </p>
      )}
      {view && (
        <>
          <ul className="tool-list" aria-label="Agents in the corral">
            {view.agents.map((agent) => (
              <li key={agent.id}>
                <label className="tool-switch">
                  <input
                    type="checkbox"
                    checked={agent.shown}
                    disabled={busy || (!agent.shown && full)}
                    onChange={() => toggle(view, agent)}
                  />
                  {agent.name}
                </label>
              </li>
            ))}
          </ul>
          <p className="tool-terms">
            {shownCount} of {view.maxShown} shown
          </p>
          <label className="tool-switch">
            <input
              type="checkbox"
              checked={view.keepFixed}
              disabled={busy}
              aria-describedby={hintId}
              onChange={() => void change(() => api.keepFixed(!view.keepFixed))}
            />
            Keep order fixed
          </label>
          <p className="tool-terms" id={hintId}>
            The drops stay where they are. Deploys still count, so turning this off puts the most
            used first again.
          </p>
        </>
      )}
      {error && (
        <p className="key-error" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}
