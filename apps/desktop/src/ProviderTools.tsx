import { useEffect, useId, useState } from "react";
import { errorText } from "./keys";
import {
  dollars,
  providerToolsApi,
  type ProviderTool,
  type ProviderToolChoice,
  type ProviderToolsApi,
  type ProviderToolsView,
} from "./providerToolsApi";

/** What a tool lets an agent do, as the switch's label. */
function sentence(choice: ProviderToolChoice): string {
  const what: Record<ProviderTool, string> = { web_search: "search the web" };
  return `${choice.agentName} can ${what[choice.tool]}`;
}

/** What the user is agreeing to, under the switch. */
function terms(choice: ProviderToolChoice): string {
  const use: Record<ProviderTool, string> = { web_search: "search" };
  return (
    `Runs on ${choice.runsOn}'s servers, where Corral can't stop it partway. ` +
    `${dollars(choice.costMicroUsd)} a ${use[choice.tool]}, at most ${choice.perTurn} a turn, ` +
    `counted in ${choice.agentName}'s spend. What it finds is untrusted, so anything that ` +
    `sends data out asks you first.`
  );
}

/**
 * The switches for tools that run on a provider's servers (TP-PROV-11, 12). Each is off until
 * turned on; the core saves the choice and the next task uses it. Given an `agent`, only its
 * switches, inside the agent editor (TP-AED-09).
 */
export function ProviderTools({
  api = providerToolsApi,
  agent,
  agentName,
}: {
  api?: ProviderToolsApi;
  agent?: string;
  agentName?: string;
}) {
  const [view, setView] = useState<ProviderToolsView | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const headingId = useId();

  useEffect(() => {
    let live = true;
    api.list().then(
      (loaded) => live && setView(loaded),
      (e: unknown) => live && setError(`Couldn't load provider tools: ${errorText(e)}`),
    );
    return () => {
      live = false;
    };
  }, [api]);

  async function toggle(choice: ProviderToolChoice) {
    setBusy(true);
    setError(null);
    try {
      setView(await api.set(choice.agent, choice.tool, !choice.on));
    } catch (e: unknown) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  }

  const problem = view?.problem ?? null;
  const choices = view?.choices.filter((c) => agent === undefined || c.agent === agent) ?? [];
  const intro = (
    <p className="key-status">
      Tools that run on a provider's servers instead of in Corral. Each is off until you turn it
      on, and your next task uses your choice.
    </p>
  );
  const list = view && (
    <>
      {agent !== undefined && choices.length === 0 && (
        <p className="tool-terms">No provider tools for {agentName ?? agent} yet.</p>
      )}
      {choices.length > 0 && (
        <ul className="tool-list">
          {choices.map((choice) => (
            <li key={`${choice.agent}-${choice.tool}`}>
              <label className="tool-switch">
                <input
                  type="checkbox"
                  checked={choice.on}
                  disabled={busy}
                  onChange={() => void toggle(choice)}
                />
                {sentence(choice)}
              </label>
              <p className="tool-terms">{terms(choice)}</p>
            </li>
          ))}
        </ul>
      )}
    </>
  );
  const alert = (error ?? problem) && (
    <p className="key-error" role="alert">
      {error ?? problem}
    </p>
  );
  if (agent !== undefined) {
    return (
      <div className="agent-tools" aria-labelledby={headingId}>
        <h3 id={headingId}>Provider tools</h3>
        {intro}
        {list}
        {alert}
      </div>
    );
  }
  return (
    <section className="key-form" aria-labelledby={headingId}>
      <h2 id={headingId}>Provider tools</h2>
      {intro}
      {list}
      {alert}
    </section>
  );
}
