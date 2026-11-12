import { act, fireEvent, render, screen } from "@testing-library/react";
import { ProviderKeyForm } from "./ProviderKeyForm";
import type { KeyApi, KeyStatus } from "./keys";

// TP-SETUP-01: the key goes to the core and never comes back; the UI shows only the core's status.
const KEY = "xai-Q7v2LmN9pRt4sWx8yZa3bCd5eFg6hJk1"; // gitleaks:allow (made-up test key)

const notSet: KeyStatus = { provider: "xai", configured: false, last4: null };
const saved: KeyStatus = { provider: "xai", configured: true, last4: "hJk1" };

function fakeApi(initial: KeyStatus, overrides: Partial<KeyApi> = {}): KeyApi & { sent: string[] } {
  const sent: string[] = [];
  return {
    sent,
    status: vi.fn(async () => initial),
    save: vi.fn(async (_provider, key: string) => {
      sent.push(key);
      return saved;
    }),
    remove: vi.fn(async () => notSet),
    ...overrides,
  };
}

async function renderForm(api: KeyApi) {
  render(<ProviderKeyForm provider="xai" name="xAI" api={api} />);
  // Let the initial status request resolve.
  await act(async () => {});
}

function expectKeyNowhere() {
  const text = document.body.textContent ?? "";
  for (let i = 0; i + 5 <= KEY.length - 4; i++) {
    expect(text).not.toContain(KEY.slice(i, i + 5));
  }
  for (const input of document.querySelectorAll("input")) {
    expect(input.value).toBe("");
  }
}

describe("ProviderKeyForm", () => {
  it("says when no key is set", async () => {
    await renderForm(fakeApi(notSet));
    expect(screen.getByRole("heading", { name: "xAI API key" })).toBeInTheDocument();
    expect(screen.getByText(/not set/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save key" })).toBeDisabled();
    expect(screen.queryByRole("button", { name: "Remove" })).not.toBeInTheDocument();
  });

  it("TP-SETUP-01 sends the key to the core, clears the field and shows only the last 4", async () => {
    const api = fakeApi(notSet);
    await renderForm(api);
    const field = screen.getByLabelText("API key");
    expect(field).toHaveAttribute("type", "password");
    expect(field).toHaveAttribute("autocomplete", "off");

    fireEvent.change(field, { target: { value: KEY } });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Save key" }));
    });

    expect(api.sent).toEqual([KEY]);
    expect(api.save).toHaveBeenCalledWith("xai", KEY);
    expect(screen.getByText(/ends in hJk1/)).toBeInTheDocument();
    expectKeyNowhere();
  });

  it("shows the core's error without the key and clears the field", async () => {
    const api = fakeApi(notSet, {
      save: vi.fn(async () => {
        throw "API keys don't contain spaces, line breaks or special characters";
      }),
    });
    await renderForm(api);
    fireEvent.change(screen.getByLabelText("API key"), { target: { value: `${KEY} x` } });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Save key" }));
    });
    expect(screen.getByRole("alert")).toHaveTextContent(/couldn't save the key/i);
    expect(screen.getByText(/not set/i)).toBeInTheDocument();
    expectKeyNowhere();
  });

  it("offers Replace and Remove once a key is saved", async () => {
    const api = fakeApi(saved);
    await renderForm(api);
    expect(screen.getByText(/ends in hJk1/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Replace key" })).toBeInTheDocument();
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Remove" }));
    });
    expect(api.remove).toHaveBeenCalledWith("xai");
    expect(screen.getByText(/not set/i)).toBeInTheDocument();
  });

  it("says so when the key store can't be reached", async () => {
    await renderForm(
      fakeApi(notSet, {
        status: vi.fn(async () => {
          throw new Error("no Tauri");
        }),
      }),
    );
    expect(screen.getByRole("alert")).toHaveTextContent(/couldn't read/i);
  });
});
