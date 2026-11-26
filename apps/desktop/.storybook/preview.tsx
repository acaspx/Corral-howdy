import type { Preview } from "@storybook/react-vite";
import "@/components/glaze/styles.css";
import "../src/styles.css";
import { GlazeRoot } from "../src/lib/GlazeRoot";

const preview: Preview = {
  parameters: {
    layout: "padded",
  },
  // Every story sits in the window's Glaze scope, as the window does (TP-GLZ-01).
  decorators: [
    (Story) => (
      <GlazeRoot>
        <Story />
      </GlazeRoot>
    ),
  ],
};

export default preview;
