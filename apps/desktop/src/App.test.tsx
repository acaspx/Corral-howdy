import { act, render, screen } from "@testing-library/react";
import { App } from "./App";
import type { KeyApi } from "./keys";

const keyApi: KeyApi = {
  status: async (provider) => ({ provider, configured: false, last4: null }),
  save: async (provider) => ({ provider, configured: true, last4: "abcd" }),
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

  it("asks for the xAI key", async () => {
    await renderApp();
    expect(screen.getByRole("heading", { level: 2, name: "xAI API key" })).toBeInTheDocument();
  });
});
