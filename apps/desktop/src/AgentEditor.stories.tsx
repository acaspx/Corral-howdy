import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, userEvent, within } from "storybook/test";
import { AgentEditor } from "./AgentEditor";
import { agentApi, agentView, gptView, never, toolsApi, toolsView } from "./fixtures/apis";

const meta = {
  title: "Main window/Agents",
  component: AgentEditor,
  args: { toolsApi: toolsApi(toolsView()) },
} satisfies Meta<typeof AgentEditor>;
export default meta;
type Story = StoryObj<typeof meta>;

export const Loading: Story = {
  args: { api: { list: () => never(), save: () => never() } },
};

/** Claude as it comes: its catalog name, Sonnet, the default cap. */
export const Defaults: Story = { args: { api: agentApi() } };

/** Renamed Ada, on Haiku, with its own $10 cap. */
export const Renamed: Story = {
  args: {
    api: agentApi([
      agentView({
        name: "Ada",
        folder: "~/Corral/Ada",
        model: "claude-haiku-5-5",
        monthlyCapMicroUsd: 10_000_000,
        spentMicroUsd: 9_870_000,
      }),
      gptView(),
    ]),
  },
};

/** An agent Corral can't run yet: no model to pick. */
export const CantRunYet: Story = { args: { api: agentApi([gptView()]) } };

/** The saved settings can't be used: the agent can't spend until they're saved again. */
export const BrokenSettings: Story = {
  args: {
    api: agentApi([
      agentView({
        problem:
          "Claude can't spend until its settings are fixed (claude.toml: unknown field `colour`). Saving here replaces them.",
      }),
    ]),
  },
};

/** A rename whose folder is taken: the core says why, and nothing changes. */
export const RenameRefused: Story = {
  args: {
    api: agentApi([agentView()], {
      save: async () =>
        Promise.reject(
          "~/Corral/Iris already exists, and it may hold another agent's files. Pick another name, or move that folder first.",
        ),
    }),
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const name = await canvas.findByRole("textbox", { name: "Name" });
    await userEvent.clear(name);
    await userEvent.type(name, "Iris");
    await userEvent.click(canvas.getByRole("button", { name: "Save Claude" }));
    await expect(await canvas.findByRole("alert")).toHaveTextContent("already exists");
  },
};

/** A cap that isn't an amount isn't sent. */
export const CapNotAnAmount: Story = {
  args: { api: agentApi() },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.type(await canvas.findByRole("textbox", { name: /Monthly cap/ }), "lots");
    await userEvent.click(canvas.getByRole("button", { name: "Save Claude" }));
    await expect(await canvas.findByRole("alert")).toHaveTextContent("an amount in dollars");
  },
};

export const CouldntLoad: Story = {
  args: {
    api: { list: async () => Promise.reject("no app data folder"), save: () => never() },
  },
};
