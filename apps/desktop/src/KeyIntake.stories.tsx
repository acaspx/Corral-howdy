import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, userEvent, within } from "storybook/test";
import { keyApi, never } from "./fixtures/apis";
import { KeyIntake } from "./KeyIntake";

const meta = {
  title: "Main window/API keys",
  component: KeyIntake,
} satisfies Meta<typeof KeyIntake>;
export default meta;
type Story = StoryObj<typeof meta>;

/** Asking the Keychain. */
export const Loading: Story = {
  args: { api: keyApi({}, { status: () => never() }) },
};

/** A fresh install: no keys yet. */
export const NoKeys: Story = { args: { api: keyApi() } };

/** Two providers connected; the field names the provider a pasted key belongs to. */
export const Connected: Story = {
  args: { api: keyApi({ xai: "7Qx2", anthropic: "Kd2r" }) },
};

/** The Keychain couldn't be read. */
export const KeychainUnavailable: Story = {
  args: {
    api: keyApi({}, { status: async () => Promise.reject("The Keychain is locked.") }),
  },
};

/** The provider refused the key: the error says so, and the key is never shown. */
export const KeyRefused: Story = {
  args: {
    api: keyApi(
      {},
      {
        connect: async () =>
          Promise.reject("xAI refused that key. Check it in your xAI console and paste it again."),
      },
    ),
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.type(await canvas.findByLabelText("API key"), "xai-not-a-real-key-0000");
    await userEvent.click(canvas.getByRole("button", { name: /Connect/ }));
    await expect(await canvas.findByRole("alert")).toHaveTextContent("refused that key");
  },
};
