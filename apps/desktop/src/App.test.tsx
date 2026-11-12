import { act, render, screen } from "@testing-library/react";
import { App } from "./App";
import type { KeyApi } from "./keys";

const keyApi: KeyApi = {
  status: async (provider) => ({ provider, configured: false, last4: null }),
  identify: async () => null,
  connect: async (provider) => ({
    status: { provider, configured: true, last4: "abcd" },
    checked: true,
  }),
  remove: async (provider) => ({ provider, configured: false, last4: null }),
};

async function renderApp() {
  render(<App keyApi={keyApi} />);
  await act(async () => {});
}

describe("App (walking skeleton)", () => {
  it("names the window and lists the deep-work sections", async () => {
    await renderApp();
    expect(screen.getByRole("heading", { level: 1, name: "Corral" })).toBeInTheDocument();
    for (const name of ["Traces", "Agents", "Policies", "Spend"]) {
      expect(screen.getByRole("heading", { level: 2, name })).toBeInTheDocument();
    }
  });

  it("is honest about what's wired up", async () => {
    await renderApp();
    expect(screen.getByRole("status")).toHaveTextContent(/walking skeleton/i);
  });

  it("asks for an API key from any provider", async () => {
    await renderApp();
    expect(screen.getByRole("heading", { level: 2, name: "API keys" })).toBeInTheDocument();
  });
});
