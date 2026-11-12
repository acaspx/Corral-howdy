import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { KeyIntake } from "./KeyIntake";
import type { Connected, KeyApi, KeyStatus, Provider } from "./keys";

// TP-SETUP-01, 02, 05: one field for any provider's key. The core suggests the provider, checks
// the key with that provider, stores it, and answers with a status; the key never comes back.
const KEY = "xai-Q7v2LmN9pRt4sWx8yZa3bCd5eFg6hJk1"; // gitleaks:allow (made-up test key)

const notSet = (provider: Provider): KeyStatus => ({ provider, configured: false, last4: null });
const set = (provider: Provider): KeyStatus => ({ provider, configured: true, last4: "hJk1" });

function fakeApi(
  configured: Provider[] = [],
  overrides: Partial<KeyApi> = {},
): KeyApi & { sent: string[] } {
  const sent: string[] = [];
  return {
    sent,
    status: vi.fn(async (p: Provider) => (configured.includes(p) ? set(p) : notSet(p))),
    identify: vi.fn(async (key: string) => (key.startsWith("xai-") ? "xai" : null)),
    connect: vi.fn(async (p: Provider, key: string): Promise<Connected> => {
      sent.push(key);
      return { status: set(p), checked: p !== "meta" };
    }),
    remove: vi.fn(async (p: Provider) => notSet(p)),
    ...overrides,
  };
}

async function renderIntake(api: KeyApi) {
  render(<KeyIntake api={api} />);
  await act(async () => {});
}

async function paste(value: string) {
  await act(async () => {
    fireEvent.change(screen.getByLabelText("API key"), { target: { value } });
  });
}

function expectKeyNowhere() {
  const text = document.body.textContent ?? "";
  for (let i = 0; i + 5 <= KEY.length - 4; i++) {
    expect(text).not.toContain(KEY.slice(i, i + 5));
  }
  expect((screen.getByLabelText("API key") as HTMLInputElement).value).toBe("");
}

describe("KeyIntake", () => {
  it("asks for a key from any provider, in a password field", async () => {
    await renderIntake(fakeApi());
    expect(screen.getByRole("heading", { level: 2, name: "API keys" })).toBeInTheDocument();
    const field = screen.getByLabelText("API key");
    expect(field).toHaveAttribute("type", "password");
    expect(field).toHaveAttribute("autocomplete", "off");
    expect(screen.getByRole("button", { name: "Connect" })).toBeDisabled();
    expect(screen.getByText(/no providers connected yet/i)).toBeInTheDocument();
  });

  it("TP-SETUP-05 names the provider the key looks like, and lets you change it", async () => {
    await renderIntake(fakeApi());
    await paste(KEY);
    expect(screen.getByText("Looks like an xAI key.")).toBeInTheDocument();
    const provider = screen.getByLabelText("Provider") as HTMLSelectElement;
    expect(provider.value).toBe("xai");
    fireEvent.change(provider, { target: { value: "anthropic" } });
    expect(provider.value).toBe("anthropic");
  });

  it("TP-SETUP-05 asks which provider when the key doesn't say", async () => {
    await renderIntake(fakeApi());
    await paste("mystery-key-123456");
    expect(screen.getByText(/which provider is this key for/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Connect" })).toBeDisabled();
    fireEvent.change(screen.getByLabelText("Provider"), { target: { value: "meta" } });
    expect(screen.getByRole("button", { name: "Connect" })).toBeEnabled();
  });

  it("TP-SETUP-02 connects through the core, clears the field and lists the provider", async () => {
    const api = fakeApi();
    await renderIntake(api);
    await paste(KEY);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    });
    expect(api.connect).toHaveBeenCalledWith("xai", KEY);
    const list = screen.getByRole("list", { name: "Connected providers" });
    expect(within(list).getByText("xAI")).toBeInTheDocument();
    expect(within(list).getByText(/ends in hJk1 · checked with xAI/)).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("xAI connected.");
    expectKeyNowhere();
  });

  it("shows the core's refusal without the key and stores nothing", async () => {
    const api = fakeApi([], {
      connect: vi.fn(async () => {
        throw "xAI didn't accept this key.";
      }),
    });
    await renderIntake(api);
    await paste(KEY);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    });
    expect(screen.getByRole("alert")).toHaveTextContent("xAI didn't accept this key.");
    expect(screen.getByText(/no providers connected yet/i)).toBeInTheDocument();
    expectKeyNowhere();
  });

  it("says when a key was saved without a check", async () => {
    await renderIntake(fakeApi());
    await paste("mystery-key-123456");
    fireEvent.change(screen.getByLabelText("Provider"), { target: { value: "meta" } });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    });
    expect(screen.getByRole("status")).toHaveTextContent(/meta connected.*couldn't check it/i);
  });

  it("lists providers already connected and removes one", async () => {
    const api = fakeApi(["anthropic"]);
    await renderIntake(api);
    const list = screen.getByRole("list", { name: "Connected providers" });
    expect(within(list).getByText("Anthropic")).toBeInTheDocument();
    await act(async () => {
      fireEvent.click(within(list).getByRole("button", { name: "Remove Anthropic key" }));
    });
    expect(api.remove).toHaveBeenCalledWith("anthropic");
    expect(screen.getByText(/no providers connected yet/i)).toBeInTheDocument();
  });

  it("says so when the key store can't be reached", async () => {
    await renderIntake(
      fakeApi([], {
        status: vi.fn(async () => {
          throw new Error("no Tauri");
        }),
      }),
    );
    expect(screen.getByRole("alert")).toHaveTextContent(/couldn't read/i);
  });
});
