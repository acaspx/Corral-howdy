import { useEffect, useId, useState, type FormEvent } from "react";
import { errorText, keyApi, type KeyApi, type KeyStatus, type Provider } from "./keys";

interface Props {
  provider: Provider;
  /** The provider's name as people know it, e.g. "xAI". */
  name: string;
  /** The core's key commands; tests pass a fake. */
  api?: KeyApi;
}

/**
 * "Add your xAI key" (SK-3). The field is cleared the moment Save is pressed, so the key doesn't
 * linger in the UI; the core validates it, stores it in the Keychain and answers with a status
 * (TP-SETUP-01). Nothing here can read a stored key back.
 */
export function ProviderKeyForm({ provider, name, api = keyApi }: Props) {
  const [status, setStatus] = useState<KeyStatus | null>(null);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const fieldId = useId();
  const headingId = useId();

  useEffect(() => {
    let live = true;
    api.status(provider).then(
      (s) => live && setStatus(s),
      (e: unknown) => live && setError(`Couldn't read the key status: ${errorText(e)}`),
    );
    return () => {
      live = false;
    };
  }, [api, provider]);

  async function save(event: FormEvent) {
    event.preventDefault();
    const key = draft;
    setDraft("");
    setBusy(true);
    setError(null);
    try {
      setStatus(await api.save(provider, key));
    } catch (e: unknown) {
      setError(`Couldn't save the key: ${errorText(e)}`);
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    setBusy(true);
    setError(null);
    try {
      setStatus(await api.remove(provider));
    } catch (e: unknown) {
      setError(`Couldn't remove the key: ${errorText(e)}`);
    } finally {
      setBusy(false);
    }
  }

  const configured = status?.configured ?? false;

  return (
    <section className="key-form" aria-labelledby={headingId}>
      <h2 id={headingId}>{name} API key</h2>
      <p className="key-status">
        {status === null
          ? "Checking…"
          : configured
            ? `Saved in your Keychain · ends in ${status.last4 ?? "····"}`
            : "Not set. Corral keeps it in your macOS Keychain and never shows it again."}
      </p>
      <form onSubmit={save}>
        <label htmlFor={fieldId}>API key</label>
        <input
          id={fieldId}
          type="password"
          autoComplete="off"
          spellCheck={false}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          disabled={busy}
        />
        <button type="submit" disabled={busy || draft.trim() === ""}>
          {configured ? "Replace key" : "Save key"}
        </button>
        {configured && (
          <button type="button" onClick={remove} disabled={busy}>
            Remove
          </button>
        )}
      </form>
      {error && (
        <p className="key-error" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}
