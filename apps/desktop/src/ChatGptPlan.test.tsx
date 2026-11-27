import { act, fireEvent, render, screen, within } from "@testing-library/react";
import type { ChatGptSignedOut, ChatGptStatus } from "./bindings";
import { ChatGptPlan } from "./ChatGptPlan";
import type { ChatGptApi } from "./chatgptApi";

// TP-SUB-05: the main window's ChatGPT plan section (ADR-017). The core signs in through the
// browser and keeps the tokens (apps/desktop/src-tauri/src/chatgpt_signin.rs); this asks for it,
// shows who is signed in and that the plan is in use, and follows OpenAI's interface guidelines:
// "Continue with ChatGPT", "Using ChatGPT plan", Manage usage, and a welcome shown once.

const signedOut: ChatGptStatus = { signedIn: false, email: null, showWelcome: false };
const signedIn: ChatGptStatus = { signedIn: true, email: "anton@example.test", showWelcome: false };
const welcome: ChatGptStatus = { ...signedIn, showWelcome: true };

/** A promise the test settles by hand. */
function later<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function fakeApi(start: ChatGptStatus, overrides: Partial<ChatGptApi> = {}): ChatGptApi {
  return {
    status: vi.fn(async () => start),
    signIn: vi.fn(async () => welcome),
    cancel: vi.fn(async () => {}),
    signOut: vi.fn(async (): Promise<ChatGptSignedOut> => ({ revoked: true, status: signedOut })),
    welcomed: vi.fn(async () => signedIn),
    manageUsage: vi.fn(async () => {}),
    ...overrides,
  };
}

async function renderPlan(api: ChatGptApi) {
  render(<ChatGptPlan api={api} />);
  await act(async () => {});
  return screen.getByRole("region", { name: "ChatGPT plan" });
}

describe("ChatGptPlan", () => {
  it("TP-SUB-05 signed out, it offers Continue with ChatGPT and waits for the browser", async () => {
    const pending = later<ChatGptStatus>();
    const api = fakeApi(signedOut, { signIn: vi.fn(() => pending.promise) });
    const section = await renderPlan(api);
    expect(section).toHaveTextContent("ChatGPT Plus or Pro");
    await act(async () => {
      fireEvent.click(within(section).getByRole("button", { name: "Continue with ChatGPT" }));
    });
    expect(api.signIn).toHaveBeenCalledTimes(1);
    expect(section).toHaveTextContent("Finish signing in in your browser");
    expect(within(section).queryByRole("button", { name: "Continue with ChatGPT" })).toBeNull();
    expect(within(section).getByRole("button", { name: "Cancel" })).toBeEnabled();

    await act(async () => {
      pending.resolve(signedIn);
    });
    expect(section).toHaveTextContent("Signed in as anton@example.test");
    expect(section).toHaveTextContent("Using ChatGPT plan");
  });

  it("TP-SUB-05 signed in, it says the plan is in use and Manage usage opens ChatGPT's settings", async () => {
    const api = fakeApi(signedIn);
    const section = await renderPlan(api);
    expect(section).toHaveTextContent("Signed in as anton@example.test");
    expect(section).toHaveTextContent("Using ChatGPT plan");
    await act(async () => {
      fireEvent.click(within(section).getByRole("button", { name: "Manage usage" }));
    });
    expect(api.manageUsage).toHaveBeenCalledTimes(1);
    expect(within(section).queryByRole("button", { name: "Continue with ChatGPT" })).toBeNull();
  });

  it("TP-SUB-05 the welcome shows once, after the first sign-in, until Got it", async () => {
    const api = fakeApi(signedOut);
    const section = await renderPlan(api);
    await act(async () => {
      fireEvent.click(within(section).getByRole("button", { name: "Continue with ChatGPT" }));
    });
    const dialog = screen.getByRole("dialog", { name: "You're using your ChatGPT plan" });
    await act(async () => {
      fireEvent.click(within(dialog).getByRole("button", { name: "Got it" }));
    });
    expect(api.welcomed).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("dialog")).toBeNull();

    const again = fakeApi(signedIn);
    await renderPlanAgain(again);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("TP-SUB-05 Cancel stops the wait and shows no error", async () => {
    const pending = later<ChatGptStatus>();
    const api = fakeApi(signedOut, {
      signIn: vi.fn(() => pending.promise),
      cancel: vi.fn(async () => {
        pending.reject("ChatGPT sign-in was cancelled");
      }),
    });
    const section = await renderPlan(api);
    await act(async () => {
      fireEvent.click(within(section).getByRole("button", { name: "Continue with ChatGPT" }));
    });
    await act(async () => {
      fireEvent.click(within(section).getByRole("button", { name: "Cancel" }));
    });
    expect(api.cancel).toHaveBeenCalledTimes(1);
    expect(within(section).queryByRole("alert")).toBeNull();
    expect(within(section).getByRole("button", { name: "Continue with ChatGPT" })).toBeEnabled();
  });

  it("TP-SUB-05 a failed sign-in says why and can be tried again", async () => {
    const api = fakeApi(signedOut, {
      signIn: vi.fn(async () => {
        throw "ChatGPT plan usage wasn't allowed, so nothing was saved.";
      }),
    });
    const section = await renderPlan(api);
    await act(async () => {
      fireEvent.click(within(section).getByRole("button", { name: "Continue with ChatGPT" }));
    });
    expect(within(section).getByRole("alert")).toHaveTextContent("plan usage wasn't allowed");
    expect(within(section).getByRole("button", { name: "Continue with ChatGPT" })).toBeEnabled();
  });

  it("TP-SUB-05 signing out, and saying so when ChatGPT didn't confirm it", async () => {
    const api = fakeApi(signedIn);
    const section = await renderPlan(api);
    await act(async () => {
      fireEvent.click(within(section).getByRole("button", { name: "Sign out" }));
    });
    expect(api.signOut).toHaveBeenCalledTimes(1);
    expect(within(section).getByRole("button", { name: "Continue with ChatGPT" })).toBeEnabled();
    expect(section).not.toHaveTextContent("didn't confirm");

    const unconfirmed = fakeApi(signedIn, {
      signOut: vi.fn(async () => ({ revoked: false, status: signedOut })),
    });
    const second = (await renderPlanAgain(unconfirmed)) as HTMLElement;
    await act(async () => {
      fireEvent.click(within(second).getByRole("button", { name: "Sign out" }));
    });
    expect(second).toHaveTextContent("ChatGPT didn't confirm");
    expect(second).toHaveTextContent("disconnect Corral in ChatGPT's settings");
  });
});

/** A second section in the same test, picked out from the first. */
async function renderPlanAgain(api: ChatGptApi) {
  render(<ChatGptPlan api={api} />);
  await act(async () => {});
  const sections = screen.getAllByRole("region", { name: "ChatGPT plan" });
  return sections[sections.length - 1];
}

describe("ChatGptPlan and the agent editor", () => {
  it("TP-SUB-08 tells the window when the sign-in changes, so GPT's models follow", async () => {
    const onChange = vi.fn();
    render(<ChatGptPlan api={fakeApi(signedOut)} onChange={onChange} />);
    await act(async () => {});
    const section = screen.getByRole("region", { name: "ChatGPT plan" });
    expect(onChange).not.toHaveBeenCalled();
    await act(async () => {
      fireEvent.click(within(section).getByRole("button", { name: "Continue with ChatGPT" }));
    });
    expect(onChange).toHaveBeenCalledTimes(1);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Got it" }));
    });
    expect(onChange).toHaveBeenCalledTimes(1);
    await act(async () => {
      fireEvent.click(within(section).getByRole("button", { name: "Sign out" }));
    });
    expect(onChange).toHaveBeenCalledTimes(2);
  });
});

