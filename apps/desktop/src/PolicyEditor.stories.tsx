import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, userEvent, within } from "storybook/test";
import { customizedPolicy, never, policyApi, policyView } from "./fixtures/apis";
import { PolicyEditor } from "./PolicyEditor";

const meta = {
  title: "Main window/Policies",
  component: PolicyEditor,
} satisfies Meta<typeof PolicyEditor>;
export default meta;
type Story = StoryObj<typeof meta>;

export const Loading: Story = {
  args: { api: { list: () => never(), save: () => never() } },
};

/** Every rule the default for every agent. */
export const Defaults: Story = { args: { api: policyApi() } };

/** Grok's own rules: writes allowed, sending blocked, a longer loop limit. */
export const OwnRules: Story = { args: { api: policyApi([customizedPolicy()]) } };

/** The saved policy can't be used: the agent has no tools until it's saved again. */
export const BrokenFile: Story = {
  args: {
    api: policyApi([
      policyView({
        problem:
          "Grok's saved policy can't be used, so Grok has no tools (unknown field `colour`). Saving here replaces it.",
      }),
    ]),
  },
};

/** A refused save: the core says why, and nothing changes. */
export const SaveRefused: Story = {
  args: {
    api: policyApi([policyView()], {
      save: async () => Promise.reject("\"https://example.com\" isn't a domain: use a name like arxiv.org"),
    }),
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(await canvas.findByRole("button", { name: "Save Grok's policy" }));
    await expect(await canvas.findByRole("alert")).toHaveTextContent("isn't a domain");
  },
};

export const CouldntLoad: Story = {
  args: {
    api: { list: async () => Promise.reject("no app data folder"), save: () => never() },
  },
};
