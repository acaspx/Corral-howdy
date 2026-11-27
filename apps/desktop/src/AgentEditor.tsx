import { useEffect, useId, useState } from "react";
import { Alert, AlertDescription } from "@/components/glaze/ui/alert";
import { Button } from "@/components/glaze/ui/button";
import { Card, CardContent, CardFooter, CardMeta } from "@/components/glaze/ui/card";
import { Field, FieldDescription, FieldLabel } from "@/components/glaze/ui/field";
import { Input } from "@/components/glaze/ui/input";
import { NativeSelect } from "@/components/glaze/ui/select";
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

/** What an agent on the ChatGPT plan says when it has no models to choose from (TP-SUB-08). */
function planNote(view: AgentEditorView): string {
  switch (view.chatgptPlan?.state) {
    case "signedOut":
      return `${view.name} runs on your ChatGPT plan. Continue with ChatGPT above to choose its model.`;
    case "unavailable":
      return view.chatgptPlan.message;
    case "signedIn":
    case undefined:
      return "Your ChatGPT plan offers no models right now.";
  }
}

/**
 * The agent editor (TP-AED-09): for each agent, its name (its folder follows), its model from
 * the priced table, its own monthly cap and what it spent this month, and the provider tools it
 * can be given. GPT's models are its ChatGPT plan's, and it has no dollar cap (TP-SUB-08). The
 * core checks and keeps a save; it applies without a restart.
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
    <section aria-labelledby={headingId} className="flex flex-col gap-3">
      <div className="flex flex-col gap-1">
        <h2 id={headingId} className="type-heading">
          Agents
        </h2>
        <p className="type-body-sm text-text-2">
          Each agent's name, the model it runs on, and how much it may spend in a month.
        </p>
      </div>
      {views && view && draft && (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void save(view, draft);
          }}
        >
          <Card>
            <CardContent className="gap-4">
              <Field>
                <FieldLabel>Agent</FieldLabel>
                <NativeSelect value={view.agent} disabled={busy} onChange={(e) => pick(e.target.value)}>
                  {views.map((v) => (
                    <option key={v.agent} value={v.agent}>
                      {v.name}
                    </option>
                  ))}
                </NativeSelect>
              </Field>
              <Field>
                <FieldLabel>Name</FieldLabel>
                <Input
                  value={draft.name}
                  maxLength={32}
                  placeholder={view.catalogName}
                  autoComplete="off"
                  disabled={busy}
                  onChange={(e) => edit({ name: e.target.value })}
                />
                <FieldDescription>
                  Works in {view.folder}. A new name starts a new folder; the old one stays as it
                  was.
                </FieldDescription>
              </Field>
              {view.chatgptPlan && view.models.length === 0 ? (
                <p className="type-body-sm text-text-2">{planNote(view)}</p>
              ) : view.models.length > 0 ? (
                <Field>
                  <FieldLabel>Model</FieldLabel>
                  <NativeSelect
                    value={draft.model ?? ""}
                    disabled={busy}
                    onChange={(e) => edit({ model: e.target.value })}
                  >
                    {view.models.map((m) => (
                      <option key={m.id} value={m.id}>
                        {m.name}
                      </option>
                    ))}
                  </NativeSelect>
                  <FieldDescription>
                    {view.models.find((m) => m.id === draft.model)?.price ?? view.models[0]?.price}
                  </FieldDescription>
                </Field>
              ) : (
                <p className="type-body-sm text-text-2">Corral can't run {view.name} yet.</p>
              )}
              {view.chatgptPlan ? (
                <p className="type-body-sm text-text-2">
                  No dollar cap: {view.name}'s tasks count against your plan's usage.
                </p>
              ) : (
                <Field>
                  <FieldLabel>Monthly cap, in dollars</FieldLabel>
                  <Input
                    inputMode="decimal"
                    value={draft.cap}
                    placeholder={capText(view.defaultCapMicroUsd)}
                    autoComplete="off"
                    disabled={busy}
                    onChange={(e) => edit({ cap: e.target.value })}
                  />
                  <FieldDescription>
                    Empty is the default, {dollars(view.defaultCapMicroUsd)}. At most{" "}
                    {dollars(view.maxCapMicroUsd)}.
                  </FieldDescription>
                </Field>
              )}
            </CardContent>
            <CardFooter>
              {view.spentMicroUsd !== null && !view.chatgptPlan && (
                <CardMeta>Spent {dollars(view.spentMicroUsd)} this month.</CardMeta>
              )}
              <Button type="submit" size="sm" pending={busy} pendingLabel="Saving">
                Save {view.name}
              </Button>
            </CardFooter>
          </Card>
        </form>
      )}
      {saved && (
        <Alert status="ok">
          <AlertDescription>{saved}</AlertDescription>
        </Alert>
      )}
      {alert && (
        <Alert status="fail">
          <AlertDescription>{alert}</AlertDescription>
        </Alert>
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
