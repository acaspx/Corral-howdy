import type { Meta, StoryObj } from "@storybook/react-vite";
import { App } from "./App";
import {
  agentApi,
  corralApi,
  keyApi,
  planApi,
  planSignedIn,
  policyApi,
  toolsApi,
  toolsView,
  tracesApi,
} from "./fixtures/apis";

// The whole main window, every section with its fixtures: what axe checks as a page, landmarks
// included (TP-A11Y-04), and what Tab walks through (TP-A11Y-05).
const meta = {
  title: "Main window/Everything",
  component: App,
  parameters: { layout: "fullscreen" },
} satisfies Meta<typeof App>;
export default meta;
type Story = StoryObj<typeof meta>;

/** Two keys connected, runs in the trace viewer, every section filled. */
export const MainWindow: Story = {
  args: {
    keyApi: keyApi({ xai: "7Qx2", anthropic: "Kd2r" }),
    toolsApi: toolsApi(toolsView(true)),
    tracesApi: tracesApi(),
    corralApi: corralApi(),
    policyApi: policyApi(),
    agentApi: agentApi(),
    planApi: planApi(planSignedIn),
  },
};

/** A fresh install: no keys, no runs. */
export const FreshInstall: Story = {
  args: {
    keyApi: keyApi(),
    toolsApi: toolsApi(),
    tracesApi: tracesApi([]),
    corralApi: corralApi(),
    policyApi: policyApi(),
    agentApi: agentApi(),
    planApi: planApi(),
  },
};

/** One agent's run open, its trace on show. */
export const RunOpen: Story = {
  args: { ...MainWindow.args, tracesApi: tracesApi(undefined, { open: "run-tidy" }) },
};
