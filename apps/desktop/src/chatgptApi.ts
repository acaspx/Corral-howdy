import { invoke } from "@tauri-apps/api/core";
import type { ChatGptSignedOut, ChatGptStatus } from "./bindings";

/**
 * The ChatGPT plan's sign-in (ADR-017). The core opens the browser at OpenAI's sign-in, waits
 * for its callback on 127.0.0.1 and keeps the tokens in the Keychain; the window only asks and
 * shows the status. No token ever comes back here.
 */
export type { ChatGptSignedOut, ChatGptStatus };

export interface ChatGptApi {
  status(): Promise<ChatGptStatus>;
  /** Opens the browser and resolves once the sign-in finished (or rejects with why not). */
  signIn(): Promise<ChatGptStatus>;
  /** Ends a sign-in still waiting for the browser. */
  cancel(): Promise<void>;
  signOut(): Promise<ChatGptSignedOut>;
  /** The welcome was seen. */
  welcomed(): Promise<ChatGptStatus>;
  /** Opens ChatGPT's usage settings. */
  manageUsage(): Promise<void>;
}

/** The core's commands (apps/desktop/src-tauri/src/chatgpt_signin.rs). */
export const chatgptApi: ChatGptApi = {
  status: () => invoke<ChatGptStatus>("chatgpt_status"),
  signIn: () => invoke<ChatGptStatus>("chatgpt_sign_in"),
  cancel: async () => {
    await invoke("chatgpt_cancel_sign_in");
  },
  signOut: () => invoke<ChatGptSignedOut>("chatgpt_sign_out"),
  welcomed: () => invoke<ChatGptStatus>("chatgpt_welcomed"),
  manageUsage: async () => {
    await invoke("open_manage_usage");
  },
};
