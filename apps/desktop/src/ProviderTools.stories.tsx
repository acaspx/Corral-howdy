import type { Meta, StoryObj } from "@storybook/react-vite";
import { never, toolsApi, toolsView } from "./fixtures/apis";
import { ProviderTools } from "./ProviderTools";

const meta = {
  title: "Main window/Provider tools",
  component: ProviderTools,
} satisfies Meta<typeof ProviderTools>;
export default meta;
type Story = StoryObj<typeof meta>;

export const Loading: Story = {
  args: { api: { list: () => never(), set: () => never() } },
};

/** Every tool off: each is off until turned on. */
export const AllOff: Story = { args: { api: toolsApi() } };

/** Grok can search the web. */
export const SearchOn: Story = { args: { api: toolsApi(toolsView(true)) } };

/** The saved choices couldn't be read, so every tool is off. */
export const SavedChoicesUnreadable: Story = {
  args: {
    api: toolsApi(
      toolsView(false, {
        problem: "provider-tools.json couldn't be read, so every provider tool is off.",
      }),
    ),
  },
};

/** Inside the agent editor: one agent's tools. */
export const OneAgent: Story = {
  args: { api: toolsApi(), agent: "claude", agentName: "Claude" },
};

/** Inside the agent editor, for an agent with none yet. */
export const OneAgentWithNone: Story = {
  args: { api: toolsApi(), agent: "gpt", agentName: "GPT" },
};
