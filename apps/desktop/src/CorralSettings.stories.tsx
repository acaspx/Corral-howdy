import type { Meta, StoryObj } from "@storybook/react-vite";
import { CorralSettings } from "./CorralSettings";
import { corralApi, corralView, crowdedCorral, never } from "./fixtures/apis";

const meta = {
  title: "Main window/Corral",
  component: CorralSettings,
} satisfies Meta<typeof CorralSettings>;
export default meta;
type Story = StoryObj<typeof meta>;

export const Loading: Story = {
  args: { api: { load: () => never(), choose: () => never(), keepFixed: () => never() } },
};

/** Four agents, all shown, ordered by use. */
export const ByUse: Story = { args: { api: corralApi() } };

/** "Keep order fixed" is on. */
export const OrderFixed: Story = { args: { api: corralApi(corralView({ keepFixed: true })) } };

/** A ninth agent: the corral holds eight, so it asks which to show. */
export const NinthAgent: Story = { args: { api: corralApi(crowdedCorral()) } };

export const CouldntLoad: Story = {
  args: {
    api: {
      load: async () => Promise.reject("The corral isn't running."),
      choose: () => never(),
      keepFixed: () => never(),
    },
  },
};
