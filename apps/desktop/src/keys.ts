import { invoke } from "@tauri-apps/api/core";

/**
 * Provider keys (SK-3). The key goes to the core, which stores it in the Keychain; only a status
 * ever comes back: whether a key is set and its last 4 characters (TP-SETUP-01, TP-SEC-02).
 */
export type Provider = "xai" | "openai" | "anthropic" | "meta";

export interface KeyStatus {
  provider: Provider;
  configured: boolean;
  last4: string | null;
}

export interface KeyApi {
  status(provider: Provider): Promise<KeyStatus>;
  save(provider: Provider, key: string): Promise<KeyStatus>;
  remove(provider: Provider): Promise<KeyStatus>;
}

/** The core's commands (apps/desktop/src-tauri/src/keys.rs). */
export const keyApi: KeyApi = {
  status: (provider) => invoke<KeyStatus>("provider_key_status", { provider }),
  save: (provider, key) => invoke<KeyStatus>("save_provider_key", { provider, key }),
  remove: (provider) => invoke<KeyStatus>("remove_provider_key", { provider }),
};

/** A command error as text. The core's messages never contain the key. */
export function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return "unknown error";
}
