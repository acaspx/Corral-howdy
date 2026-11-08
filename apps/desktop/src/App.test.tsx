import { render, screen } from "@testing-library/react";
import { App } from "./App";

describe("App (M0 skeleton)", () => {
  it("names the window and lists the deep-work sections", () => {
    render(<App />);
    expect(screen.getByRole("heading", { level: 1, name: "Corral" })).toBeInTheDocument();
    for (const name of ["Traces", "Agents", "Policies", "Spend"]) {
      expect(screen.getByRole("heading", { level: 2, name })).toBeInTheDocument();
    }
  });

  it("is honest that nothing is wired up yet", () => {
    render(<App />);
    expect(screen.getByRole("status")).toHaveTextContent(/milestone M0/);
  });
});
