import { invoke } from "@tauri-apps/api/core";

/**
 * Provider keys. One field takes any provider's key: the core suggests the provider from the key
 * (TP-SETUP-05), checks it with that provider and stores it in the Keychain (TP-SETUP-02). Only a
 * status ever comes back: whether a key is set and its last 4 characters (TP-SETUP-01, TP-SEC-02).
 */
export type Provider = "xai" | "openai" | "anthropic" | "meta";

/** Providers in the order the picker lists them, with the names people know them by. */
export const PROVIDERS: readonly { id: Provider; name: string }[] = [
  { id: "xai", name: "xAI" },
  { id: "anthropic", name: "Anthropic" },
  { id: "openai", name: "OpenAI" },
  { id: "meta", name: "Meta" },
];

export function providerName(provider: Provider): string {
  return PROVIDERS.find((p) => p.id === provider)?.name ?? provider;
}

export interface KeyStatus {
  provider: Provider;
  configured: boolean;
  last4: string | null;
}

export interface Connected {
  status: KeyStatus;
  /** Whether the provider accepted the key; false when there's no check for it yet. */
  checked: boolean;
}

export interface KeyApi {
  status(provider: Provider): Promise<KeyStatus>;
  identify(key: string): Promise<Provider | null>;
  connect(provider: Provider, key: string): Promise<Connected>;
  remove(provider: Provider): Promise<KeyStatus>;
}

/** The core's commands (apps/desktop/src-tauri/src/keys.rs). */
export const keyApi: KeyApi = {
  status: (provider) => invoke<KeyStatus>("provider_key_status", { provider }),
  identify: (key) => invoke<Provider | null>("identify_provider_key", { key }),
  connect: (provider, key) => invoke<Connected>("connect_provider_key", { provider, key }),
  remove: (provider) => invoke<KeyStatus>("remove_provider_key", { provider }),
};

/** A command error as text. The core's messages never contain the key. */
export function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return "unknown error";
}
