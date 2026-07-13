import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ModelSnapshot } from "../../protocol";

const MODEL: ModelSnapshot = {
  current: { id: "gpt-5.6-sol", variant: "medium" },
  available: [
    {
      id: "gpt-5.6-sol",
      label: "GPT-5.6 Sol",
      variants: ["low", "medium", "high", "xhigh", "max"],
      default_variant: "medium",
    },
  ],
};

const setModel = vi.fn();

beforeEach(() => {
  vi.resetModules();
  setModel.mockReset();
  vi.doMock("../../ws/client", () => ({ getClient: () => ({ setModel }) }));
});

afterEach(() => vi.unstubAllGlobals());

async function mount(seedModel?: ModelSnapshot) {
  const store = await import("../../store/store");
  store.dispatch({
    type: "hello_ok",
    payload: {
      type: "hello_ok",
      latest_msg_id: 0,
      messages: [],
      pings: [],
      ...(seedModel ? { model: seedModel } : {}),
    },
  });
  const { ModelChip } = await import("./ModelChip");
  return render(() => <ModelChip />);
}

describe("ModelChip (chat header)", () => {
  it("renders the current model + variant", async () => {
    const { getByText } = await mount(MODEL);
    expect(getByText("GPT-5.6 Sol")).toBeTruthy();
    expect(getByText("medium")).toBeTruthy();
  });

  it("renders nothing when no model snapshot is present (older host)", async () => {
    const { container } = await mount();
    expect(container.textContent).toBe("");
  });

  it("opens the popover and enqueues set_model when a variant is picked", async () => {
    await mount(MODEL);
    // Open the popover from the chip trigger.
    fireEvent.click(screen.getByRole("button"));
    const highOption = await waitFor(() => screen.getByRole("radio", { name: "high" }));
    fireEvent.click(highOption);
    expect(setModel).toHaveBeenCalledWith("gpt-5.6-sol", "high");
  });
});
