import { beforeEach, describe, expect, it, vi } from "vitest";

// Fresh store + mocked ws client per test (the shared singleton pattern).
beforeEach(() => {
  vi.resetModules();
});

describe("decideEventWithUndo — the event_action round-trip", () => {
  it("flips the event decided optimistically and posts the exact event_action payload", async () => {
    const store = await import("../store/store");
    const sent: unknown[] = [];
    vi.doMock("../ws/client", () => ({
      getClient: () => ({
        sendEventAction: (eventId: number, action: string, data: unknown) =>
          sent.push({ type: "event_action", event_id: eventId, action, data }),
      }),
    }));
    const { decideEventWithUndo } = await import("./event-decide");

    const payload = decideEventWithUndo(42, "choose", { choice: "A" }, "New reopen_ping op");

    // Optimistic decide assertion recorded at once, in the ONE override record.
    expect(store.state.eventOverrides).toEqual({ 42: { decided: true } });
    // The wire payload matches the contract shape exactly.
    expect(payload).toEqual({ type: "event_action", event_id: 42, action: "choose", data: { choice: "A" } });
    expect(sent).toEqual([{ type: "event_action", event_id: 42, action: "choose", data: { choice: "A" } }]);
  });

  it("reopens the event on Undo, dropping the optimistic override and posting reopen", async () => {
    const store = await import("../store/store");
    const sent: { action: string; eventId: number }[] = [];
    vi.doMock("../ws/client", () => ({
      getClient: () => ({
        sendEventAction: (eventId: number, action: string) => sent.push({ action, eventId }),
      }),
    }));
    const { decideEventWithUndo, reopenEvent } = await import("./event-decide");

    decideEventWithUndo(7, "choose", { choice: "B" });
    expect(store.state.eventOverrides).toEqual({ 7: { decided: true } });

    // Reopen asserts "open" again — which the wire truth already says, so the
    // assertion settles away on the spot and leaves no residue.
    reopenEvent(7);
    expect(store.state.eventOverrides).toEqual({});
    expect(sent).toEqual([
      { action: "choose", eventId: 7 },
      { action: "reopen", eventId: 7 },
    ]);
  });
});
