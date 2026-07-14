import { render } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";

// AgentStatus is now EXCEPTION-ONLY (status/meta decruft). The header no longer
// carries "Agent · idle" furniture, and it never doubles the inline transcript
// "Thinking…" marker — a live turn is announced exactly once, where the eye is.
// The only thing it renders is the exceptional offline reading. Fresh store
// singleton per test.
beforeEach(() => {
  vi.resetModules();
});

async function mount(opts: { connection: "connected" | "reconnecting"; thinking?: boolean }) {
  const store = await import("../../store/store");
  store.dispatch({ type: "connection_status", status: opts.connection });
  store.dispatch({
    type: "agent_activity",
    payload: opts.thinking ? { state: "thinking", text: "Thinking…" } : { state: "idle", text: null },
  });
  const { AgentStatus } = await import("./AgentStatus");
  return render(() => <AgentStatus />);
}

describe("AgentStatus — exception-only header datum", () => {
  it("renders NOTHING when connected and idle (no standing furniture)", async () => {
    const { container } = await mount({ connection: "connected" });
    expect(container.textContent).toBe("");
  });

  it("renders NOTHING while thinking (the transcript marker is the one indicator)", async () => {
    const { container } = await mount({ connection: "connected", thinking: true });
    // Never a second "thinking" reading in the header.
    expect(container.textContent).toBe("");
  });

  it("renders the offline reading only in the exceptional disconnected state", async () => {
    const { container } = await mount({ connection: "reconnecting", thinking: true });
    // Exception still renders — but it never lies "thinking" while the socket is down.
    expect(container.textContent).toContain("offline");
    expect(container.textContent?.toLowerCase()).not.toContain("thinking");
  });
});
