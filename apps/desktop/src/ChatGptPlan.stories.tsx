import type { Meta, StoryObj } from "@storybook/react-vite";
import { ChatGptPlan } from "./ChatGptPlan";
import { never, planApi, planSignedIn } from "./fixtures/apis";

const meta = {
  title: "Main window/ChatGPT plan",
  component: ChatGptPlan,
} satisfies Meta<typeof ChatGptPlan>;
export default meta;
type Story = StoryObj<typeof meta>;

export const Loading: Story = { args: { api: planApi(undefined, { status: () => never() }) } };

/** Not signed in: Continue with ChatGPT. */
export const SignedOut: Story = { args: { api: planApi() } };

/** Signed in: the account, Using ChatGPT plan, Manage usage and Sign out. */
export const SignedIn: Story = { args: { api: planApi(planSignedIn) } };

/** Just signed in for the first time: the one-time welcome. */
export const Welcome: Story = { args: { api: planApi({ ...planSignedIn, showWelcome: true }) } };

export const CouldntLoad: Story = {
  args: { api: planApi(undefined, { status: async () => Promise.reject("The Keychain refused.") }) },
};
