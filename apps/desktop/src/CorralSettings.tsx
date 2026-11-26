import { useEffect, useId, useState } from "react";
import { Alert, AlertDescription } from "@/components/glaze/ui/alert";
import { Card, CardContent } from "@/components/glaze/ui/card";
import { Checkbox } from "@/components/glaze/ui/checkbox";
import {
  Field,
  FieldContent,
  FieldDescription,
  FieldLabel,
  FieldLegend,
  FieldSet,
} from "@/components/glaze/ui/field";
import { Switch } from "@/components/glaze/ui/switch";
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
    <section aria-labelledby={headingId} className="flex flex-col gap-3">
      <div className="flex flex-col gap-1">
        <h2 id={headingId} className="type-heading">
          Corral
        </h2>
        <p className="type-body-sm text-text-2">
          The corral shows up to {view?.maxShown ?? 8} of your agents, most used first. Agents you
          don't show stay connected.
        </p>
      </div>
      {view?.needsChoice && (
        <Alert status="warn" role="status">
          <AlertDescription>
            Your corral holds up to {view.maxShown}. Choose which agents show.
          </AlertDescription>
        </Alert>
      )}
      {view && (
        <Card>
          <CardContent className="gap-4">
            <FieldSet>
              <FieldLegend variant="label">Agents in the corral</FieldLegend>
              <div className="flex flex-col gap-2">
                {view.agents.map((agent) => (
                  <Field key={agent.id} orientation="horizontal">
                    <Checkbox
                      checked={agent.shown}
                      disabled={busy || (!agent.shown && full)}
                      onCheckedChange={() => toggle(view, agent)}
                    />
                    <FieldLabel>{agent.name}</FieldLabel>
                  </Field>
                ))}
              </div>
              <FieldDescription>
                {shownCount} of {view.maxShown} shown
              </FieldDescription>
            </FieldSet>
            <Field orientation="horizontal">
              <Switch
                checked={view.keepFixed}
                disabled={busy}
                onCheckedChange={() => void change(() => api.keepFixed(!view.keepFixed))}
              />
              <FieldContent>
                <FieldLabel>Keep order fixed</FieldLabel>
                <FieldDescription>
                  The drops stay where they are. Deploys still count, so turning this off puts the
                  most used first again.
                </FieldDescription>
              </FieldContent>
            </Field>
          </CardContent>
        </Card>
      )}
      {error && (
        <Alert status="fail">
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}
    </section>
  );
}
