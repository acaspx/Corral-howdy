import type { StorybookConfig } from "@storybook/react-vite";

// The main window's components, one story per state (M4 item 7). The corral itself is drawn in
// Swift; its states are in CorralPlayground (native/CorralNative).
const config: StorybookConfig = {
  stories: ["../src/**/*.stories.tsx"],
  framework: { name: "@storybook/react-vite", options: {} },
  core: { disableTelemetry: true },
};

export default config;
