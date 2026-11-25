import { useEffect, useId, useState } from "react";
import { errorText } from "./keys";
import {
  agentEditorApi,
  capFromText,
  capText,
  type AgentEditorApi,
  type AgentEditorView,
} from "./agentEditorApi";
import { ProviderTools } from "./ProviderTools";
import { dollars, providerToolsApi, type ProviderToolsApi } from "./providerToolsApi";

interface Draft {
  name: string;
  model: string | null;
  cap: string;
}

function draftOf(view: AgentEditorView): Draft {
  return { name: view.name, model: view.model, cap: capText(view.monthlyCapMicroUsd) };
}

/**
 * The agent editor (TP-AED-09): for each agent, its name (its folder follows), its model from
 * the priced table, its own monthly cap and what it spent this month, and the provider tools it
 * can be given. The core checks and keeps a save; it applies without a restart.
 */
export function AgentEditor({
  api = agentEditorApi,
  toolsApi = providerToolsApi,
}: {
  api?: AgentEditorApi;
  toolsApi?: ProviderToolsApi;
}) {
  const [views, setViews] = useState<AgentEditorView[] | null>(null);
  const [agent, setAgent] = useState<string | null>(null);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const headingId = useId();
  const ids = useId();

  useEffect(() => {
    let live = true;
    api.list().then(
      (loaded) => {
        if (!live) return;
        setViews(loaded);
        const first = loaded[0];
        if (first) {
          setAgent(first.agent);
          setDraft(draftOf(first));
        }
      },
      (e: unknown) => live && setError(`Couldn't load the agents: ${errorText(e)}`),
    );
    return () => {
      live = false;
    };
  }, [api]);

  const view = views?.find((v) => v.agent === agent) ?? null;

  function pick(next: string) {
    const chosen = views?.find((v) => v.agent === next);
    if (!chosen) return;
    setAgent(next);
    setDraft(draftOf(chosen));
    setError(null);
    setSaved(null);
  }

  function edit(change: Partial<Draft>) {
    setDraft((d) => (d ? { ...d, ...change } : d));
    setSaved(null);
  }

  async function save(current: AgentEditorView, form: Draft) {
    const cap = capFromText(form.cap);
    if (cap === undefined) {
      setSaved(null);
      setError("The monthly cap has to be an amount in dollars, like 25 or 7.50.");
      return;
    }
    const name = form.name.trim();
    setBusy(true);
    setError(null);
    setSaved(null);
    try {
      const kept = await api.save(current.agent, {
        name: name === "" ? null : name,
        model: form.model,
        monthlyCapMicroUsd: cap,
      });
      setViews((vs) => vs?.map((v) => (v.agent === kept.agent ? kept : v)) ?? vs);
      setDraft(draftOf(kept));
      setSaved(`Saved. ${kept.name}'s next task uses it.`);
    } catch (e: unknown) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  }

  const alert = error ?? view?.problem ?? null;
  return (
    <section className="key-form agent-editor" aria-labelledby={headingId}>
      <h2 id={headingId}>Agents</h2>
      <p className="key-status">
        Each agent's name, the model it runs on, and how much it may spend in a month.
      </p>
      {views && view && draft && (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void save(view, draft);
          }}
        >
          <label htmlFor={`${ids}-agent`}>Agent</label>
          <select
            id={`${ids}-agent`}
            value={view.agent}
            disabled={busy}
            onChange={(e) => pick(e.target.value)}
          >
            {views.map((v) => (
              <option key={v.agent} value={v.agent}>
                {v.name}
              </option>
            ))}
          </select>
          <label htmlFor={`${ids}-name`}>Name</label>
          <input
            id={`${ids}-name`}
            type="text"
            value={draft.name}
            maxLength={32}
            placeholder={view.catalogName}
            disabled={busy}
            onChange={(e) => edit({ name: e.target.value })}
          />
          <p className="tool-terms">
            Works in {view.folder}. A new name starts a new folder; the old one stays as it was.
          </p>
          {view.models.length > 0 ? (
            <>
              <label htmlFor={`${ids}-model`}>Model</label>
              <select
                id={`${ids}-model`}
                value={draft.model ?? ""}
                disabled={busy}
                onChange={(e) => edit({ model: e.target.value })}
              >
                {view.models.map((m) => (
                  <option key={m.id} value={m.id}>
                    {`${m.name}: ${m.price}`}
                  </option>
                ))}
              </select>
            </>
          ) : (
            <p className="tool-terms">Corral can't run {view.name} yet.</p>
          )}
          <label htmlFor={`${ids}-cap`}>Monthly cap, in dollars</label>
          <input
            id={`${ids}-cap`}
            type="text"
            inputMode="decimal"
            value={draft.cap}
            placeholder={capText(view.defaultCapMicroUsd)}
            disabled={busy}
            onChange={(e) => edit({ cap: e.target.value })}
          />
          <p className="tool-terms">
            Empty is the default, {dollars(view.defaultCapMicroUsd)}. At most{" "}
            {dollars(view.maxCapMicroUsd)}.
            {view.spentMicroUsd !== null && ` Spent ${dollars(view.spentMicroUsd)} this month.`}
          </p>
          <button type="submit" disabled={busy}>
            Save {view.name}
          </button>
        </form>
      )}
      {saved && (
        <p className="key-status" role="status">
          {saved}
        </p>
      )}
      {alert && (
        <p className="key-error" role="alert">
          {alert}
        </p>
      )}
      {view && (
        <ProviderTools
          key={`${view.agent}-${view.name}`}
          api={toolsApi}
          agent={view.agent}
          agentName={view.name}
        />
      )}
    </section>
  );
}
