import { useEffect, useId, useState } from "react";
import { errorText } from "./keys";
import {
  policyApi,
  type AgentPolicyView,
  type PolicyApi,
  type PolicyClassView,
  type RuleChoice,
  type TimeoutChoice,
} from "./policyApi";

/** A rule as a word, most permissive first. */
const WORDS: Record<RuleChoice, string> = { allow: "Allow", ask: "Ask", deny: "Block" };
const MOST_FIRST: RuleChoice[] = ["allow", "ask", "deny"];
const RANK: Record<RuleChoice, number> = { deny: 0, ask: 1, allow: 2 };

/** A class's select: its own rule, or "default". */
type Choice = RuleChoice | "default";

interface Draft {
  rules: Record<string, Choice>;
  domains: string;
  timeout: string;
  onTimeout: TimeoutChoice;
  loopLimit: string;
}

function draftOf(view: AgentPolicyView): Draft {
  return {
    rules: Object.fromEntries(view.classes.map((c) => [c.class, c.own ?? "default"])),
    domains: view.netAllow.join("\n"),
    timeout: view.checkpointTimeout,
    onTimeout: view.onTimeout,
    loopLimit: String(view.loopLimit),
  };
}

/** Where a class's decision comes from, in plain words. */
function why(c: PolicyClassView, agentName: string): string {
  switch (c.origin) {
    case "default":
      return "Default for every agent";
    case "agent":
      return `Set for ${agentName}`;
    case "ceiling":
      return `At most ${WORDS[c.ceiling]} for any agent`;
    case "fixed":
      return "Always blocked";
  }
}

/**
 * The policy editor (TP-PED-09): for each agent, every action class with what it decides and
 * why, the web domains it may read without asking, how long a checkpoint waits and the loop
 * limit. Nothing above a class's ceiling is offered; the core checks and keeps the edit, and the
 * agent's next task uses it.
 */
export function PolicyEditor({ api = policyApi }: { api?: PolicyApi }) {
  const [views, setViews] = useState<AgentPolicyView[] | null>(null);
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
      (e: unknown) => live && setError(`Couldn't load the policies: ${errorText(e)}`),
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

  async function save(current: AgentPolicyView, form: Draft) {
    setBusy(true);
    setError(null);
    setSaved(null);
    try {
      const kept = await api.save(current.agent, {
        rules: current.classes
          .filter((c) => c.editable)
          .map((c) => {
            const choice = form.rules[c.class] ?? "default";
            return { class: c.class, rule: choice === "default" ? null : choice };
          }),
        netAllow: form.domains
          .split("\n")
          .map((d) => d.trim())
          .filter((d) => d.length > 0),
        checkpointTimeout: form.timeout.trim(),
        onTimeout: form.onTimeout,
        loopLimit: Number.parseInt(form.loopLimit, 10) || 0,
      });
      setViews((vs) => vs?.map((v) => (v.agent === kept.agent ? kept : v)) ?? vs);
      setDraft(draftOf(kept));
      setSaved(`Saved. ${kept.agentName}'s next task uses it.`);
    } catch (e: unknown) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  }

  const alert = error ?? view?.problem ?? null;
  return (
    <section className="key-form policy-editor" aria-labelledby={headingId}>
      <h2 id={headingId}>Policies</h2>
      <p className="key-status">
        What each agent may do on its own, what stops for your approval, and what's blocked.
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
                {v.agentName}
              </option>
            ))}
          </select>
          <table className="policy-rules">
            <thead>
              <tr>
                <th scope="col">Action</th>
                <th scope="col">Decides</th>
                <th scope="col">Why</th>
                <th scope="col">Rule</th>
              </tr>
            </thead>
            <tbody>
              {view.classes.map((c) => (
                <tr key={c.class}>
                  <th scope="row">{c.title}</th>
                  <td>{WORDS[c.rule]}</td>
                  <td>{why(c, view.agentName)}</td>
                  <td>
                    {c.editable ? (
                      <select
                        aria-label={c.title}
                        value={draft.rules[c.class] ?? "default"}
                        disabled={busy}
                        onChange={(e) =>
                          edit({ rules: { ...draft.rules, [c.class]: e.target.value as Choice } })
                        }
                      >
                        <option value="default">Default ({WORDS[c.default]})</option>
                        {MOST_FIRST.filter((r) => RANK[r] <= RANK[c.ceiling]).map((r) => (
                          <option key={r} value={r}>
                            {WORDS[r]}
                          </option>
                        ))}
                      </select>
                    ) : c.origin === "fixed" ? null : (
                      <span>Set under Provider tools</span>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          <label htmlFor={`${ids}-domains`}>Allowed web domains, one a line</label>
          <textarea
            id={`${ids}-domains`}
            rows={3}
            value={draft.domains}
            disabled={busy}
            placeholder="*.wikipedia.org"
            onChange={(e) => edit({ domains: e.target.value })}
          />
          <label htmlFor={`${ids}-timeout`}>Checkpoint wait, like 30m or 2h</label>
          <input
            id={`${ids}-timeout`}
            type="text"
            value={draft.timeout}
            disabled={busy}
            onChange={(e) => edit({ timeout: e.target.value })}
          />
          <label htmlFor={`${ids}-on-timeout`}>If nobody answers</label>
          <select
            id={`${ids}-on-timeout`}
            value={draft.onTimeout}
            disabled={busy}
            onChange={(e) => edit({ onTimeout: e.target.value as TimeoutChoice })}
          >
            <option value="deny">Block the action</option>
            <option value="cancelRun">Cancel the task</option>
          </select>
          <label htmlFor={`${ids}-loop`}>Loop limit: identical calls before each asks</label>
          <input
            id={`${ids}-loop`}
            type="number"
            min={2}
            max={1000}
            value={draft.loopLimit}
            disabled={busy}
            onChange={(e) => edit({ loopLimit: e.target.value })}
          />
          <button type="submit" disabled={busy}>
            Save {view.agentName}'s policy
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
    </section>
  );
}
