import type { Meta, StoryObj } from "@storybook/react-vite";
import { fixtureRuns, never, tracesApi } from "./fixtures/apis";
import { TraceViewer } from "./TraceViewer";

// The runs are real event streams: played by the FakeAdapter through the supervisor, the tool
// gate and a checkpoint desk into the store (src-tauri/tests/story_fixtures.rs, TP-SB-01).
const meta = {
  title: "Main window/Traces",
  component: TraceViewer,
} satisfies Meta<typeof TraceViewer>;
export default meta;
type Story = StoryObj<typeof meta>;

export const Loading: Story = {
  args: { api: { runs: () => never(), trace: () => never(), copy: async () => {} } },
};

/** No runs yet. */
export const Empty: Story = { args: { api: tracesApi([]) } };

/** The newest runs: running, cancelled, failed and completed. */
export const RunList: Story = { args: { api: tracesApi() } };

/** A run with a file read, a checkpoint approved and a file written. */
export const ApprovedCheckpoint: Story = { args: { api: tracesApi(fixtureRuns, { open: "run-tidy" }) } };

/** A send that was denied, with the person's note. */
export const DeniedCheckpoint: Story = { args: { api: tracesApi(fixtureRuns, { open: "run-email" }) } };

/** The provider failed mid-run. */
export const Failed: Story = { args: { api: tracesApi(fixtureRuns, { open: "run-flights" }) } };

/** Stopped from the hover card. */
export const Stopped: Story = { args: { api: tracesApi(fixtureRuns, { open: "run-research" }) } };

/** Still going: waiting at a checkpoint for an answer. */
export const WaitingForApproval: Story = {
  args: { api: tracesApi(fixtureRuns, { open: "run-itinerary" }) },
};

export const CouldntLoad: Story = {
  args: {
    api: {
      runs: async () => Promise.reject("The event store couldn't be opened."),
      trace: () => never(),
      copy: async () => {},
    },
  },
};
