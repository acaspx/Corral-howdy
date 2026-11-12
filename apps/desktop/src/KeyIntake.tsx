import { useEffect, useId, useState, type FormEvent } from "react";
import {
  PROVIDERS,
  errorText,
  keyApi,
  providerName,
  type KeyApi,
  type KeyStatus,
  type Provider,
} from "./keys";

/**
 * "API keys": one field for any provider's key. As you paste, the core suggests the provider
 * from the key (you can change it). Connect sends the key to the core, which checks it with the
 * provider and stores it in the Keychain; the field is cleared at once, and only a status comes
 * back (TP-SETUP-01, 02, 05). Nothing here can read a stored key back.
 */
export function KeyIntake({ api = keyApi }: { api?: KeyApi }) {
  const [statuses, setStatuses] = useState<KeyStatus[] | null>(null);
  const [draft, setDraft] = useState("");
  const [guess, setGuess] = useState<Provider | null>(null);
  const [provider, setProvider] = useState<Provider | "">("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const fieldId = useId();
  const pickerId = useId();
  const headingId = useId();

  useEffect(() => {
    let live = true;
    Promise.all(PROVIDERS.map((p) => api.status(p.id))).then(
      (all) => live && setStatuses(all),
      (e: unknown) => live && setError(`Couldn't read your keys: ${errorText(e)}`),
    );
    return () => {
      live = false;
    };
  }, [api]);

  async function typed(value: string) {
    setDraft(value);
    setNotice(null);
    if (value.trim() === "") {
      setGuess(null);
      setProvider("");
      return;
    }
    try {
      const found = await api.identify(value);
      setGuess(found);
      setProvider(found ?? "");
    } catch {
      setGuess(null);
    }
  }

  function update(status: KeyStatus) {
    setStatuses((all) => (all ?? []).map((s) => (s.provider === status.provider ? status : s)));
  }

  async function connect(event: FormEvent) {
    event.preventDefault();
    if (provider === "") return;
    const key = draft;
    setDraft("");
    setGuess(null);
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const result = await api.connect(provider, key);
      update(result.status);
      const name = providerName(provider);
      setNotice(
        result.checked
          ? `${name} connected.`
          : `${name} connected. Corral couldn't check it yet, so it'll find out on the first task.`,
      );
    } catch (e: unknown) {
      setError(errorText(e));
    } finally {
      setProvider("");
      setBusy(false);
    }
  }

  async function remove(target: Provider) {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      update(await api.remove(target));
    } catch (e: unknown) {
      setError(`Couldn't remove the key: ${errorText(e)}`);
    } finally {
      setBusy(false);
    }
  }

  const connected = (statuses ?? []).filter((s) => s.configured);
  const hint =
    draft.trim() === ""
      ? "Paste a key from xAI, Anthropic, OpenAI or Meta. Corral keeps it in your macOS Keychain and never shows it again."
      : guess
        ? `Looks like ${guess === "meta" ? "a" : "an"} ${providerName(guess)} key.`
        : "Which provider is this key for?";

  return (
    <section className="key-form" aria-labelledby={headingId}>
      <h2 id={headingId}>API keys</h2>
      <p className="key-status">{hint}</p>
      <form onSubmit={connect}>
        <label htmlFor={fieldId}>API key</label>
        <input
          id={fieldId}
          type="password"
          autoComplete="off"
          spellCheck={false}
          value={draft}
          onChange={(e) => void typed(e.target.value)}
          disabled={busy}
        />
        <label htmlFor={pickerId}>Provider</label>
        <select
          id={pickerId}
          value={provider}
          onChange={(e) => setProvider(e.target.value as Provider | "")}
          disabled={busy || draft.trim() === ""}
        >
          <option value="">Choose…</option>
          {PROVIDERS.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
        <button type="submit" disabled={busy || draft.trim() === "" || provider === ""}>
          {busy ? "Checking…" : "Connect"}
        </button>
      </form>
      {statuses === null ? null : connected.length === 0 ? (
        <p className="key-empty">No providers connected yet.</p>
      ) : (
        <ul className="key-list" aria-label="Connected providers">
          {connected.map((s) => (
            <li key={s.provider}>
              <span className="key-name">{providerName(s.provider)}</span>
              <span className="key-detail">
                ends in {s.last4 ?? "····"} ·{" "}
                {s.provider === "meta" ? "not checked yet" : `checked with ${providerName(s.provider)}`}
              </span>
              <button
                type="button"
                onClick={() => void remove(s.provider)}
                disabled={busy}
                aria-label={`Remove ${providerName(s.provider)} key`}
              >
                Remove
              </button>
            </li>
          ))}
        </ul>
      )}
      {notice && (
        <p className="key-notice" role="status">
          {notice}
        </p>
      )}
      {error && (
        <p className="key-error" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}
