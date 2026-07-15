import { beforeEach, describe, expect, it, vi } from "vitest";

// Fresh store + mocked ws client per test (the shared singleton pattern,
// mirroring event-decide.test.ts).
beforeEach(() => {
  vi.resetModules();
});

describe("archiveEventWithUndo — the event_action archive round-trip", () => {
  it("sweeps the event optimistically and posts the exact contract envelope (data is {})", async () => {
    const store = await import("../store/store");
    const sent: unknown[] = [];
    vi.doMock("../ws/client", () => ({
      getClient: () => ({
        sendEventAction: (eventId: number, action: string, data: unknown) =>
          sent.push({ type: "event_action", event_id: eventId, action, data }),
      }),
    }));
    const { archiveEventWithUndo } = await import("./event-archive");

    const payload = archiveEventWithUndo(42, { silent: true });

    // Optimistic archive override recorded at once — the default filter hides
    // the event everywhere without waiting for the host echo.
    expect(store.state.eventArchiveOverrides).toEqual([42]);
    // The wire envelope matches the contract shape exactly: data is `{}`, not null.
    expect(payload).toEqual({ type: "event_action", event_id: 42, action: "archive", data: {} });
    expect(sent).toEqual([{ type: "event_action", event_id: 42, action: "archive", data: {} }]);
  });

  it("unarchiveEvent posts unarchive and returns the event to the resting queue", async () => {
    const store = await import("../store/store");
    const sent: { action: string; eventId: number; data: unknown }[] = [];
    vi.doMock("../ws/client", () => ({
      getClient: () => ({
        sendEventAction: (eventId: number, action: string, data: unknown) =>
          sent.push({ action, eventId, data }),
      }),
    }));
    const { archiveEventWithUndo, unarchiveEvent } = await import("./event-archive");

    archiveEventWithUndo(7, { silent: true });
    expect(store.state.eventArchiveOverrides).toEqual([7]);

    unarchiveEvent(7);
    expect(store.state.eventArchiveOverrides).toEqual([]);
    expect(sent).toEqual([
      { action: "archive", eventId: 7, data: {} },
      { action: "unarchive", eventId: 7, data: {} },
    ]);
  });

  it("archiving an OPEN judgment then tapping Undo unarchives AND reopens (clearing the overrides)", async () => {
    const store = await import("../store/store");
    const toast = await import("./toast");
    const sent: { action: string; eventId: number; data: unknown }[] = [];
    vi.doMock("../ws/client", () => ({
      getClient: () => ({
        sendEventAction: (eventId: number, action: string, data: unknown) =>
          sent.push({ action, eventId, data }),
        readEvent: () => {},
      }),
    }));
    const { archiveEventWithUndo } = await import("./event-archive");
    const openJudgment = {
      id: 11,
      kind: "judgment" as const,
      source: { kind: "agent" as const, ref: "host" },
      name: "@decide-me",
      description: "decide me",
      ui: [],
      requires_response: true,
      quick_replies: [],
      status: "open" as const,
      read: false,
      anchor: 0,
      ts: "2026-07-13T09:00:00Z",
    };
    store.dispatch({ type: "event_upsert", payload: { type: "event_upsert", event: openJudgment } });

    // Archive posts the envelope and sweeps optimistically.
    archiveEventWithUndo(11);
    expect(store.state.eventArchiveOverrides).toEqual([11]);

    // Tap the "Archived" toast's Undo: because the card was still open when
    // archived (archiving auto-dismissed it host-side), Undo must restore it
    // fully — unarchive AND reopen.
    const t = toast.toasts().find((x) => x.message === "Archived");
    t!.action!.onClick();

    expect(sent).toEqual([
      { action: "archive", eventId: 11, data: {} },
      { action: "unarchive", eventId: 11, data: {} },
      { action: "reopen", eventId: 11, data: null },
    ]);
    // Both optimistic layers dropped: the event stands open in the queue again.
    expect(store.state.eventArchiveOverrides).toEqual([]);
    expect(store.state.eventDecideOverrides).toEqual([]);
  });

  it("archiving a DONE event then tapping Undo unarchives only (never reopens)", async () => {
    const store = await import("../store/store");
    const toast = await import("./toast");
    const sent: { action: string; eventId: number; data: unknown }[] = [];
    vi.doMock("../ws/client", () => ({
      getClient: () => ({
        sendEventAction: (eventId: number, action: string, data: unknown) =>
          sent.push({ action, eventId, data }),
        readEvent: () => {},
      }),
    }));
    const { archiveEventWithUndo } = await import("./event-archive");
    const doneEvent = {
      id: 12,
      kind: "summary" as const,
      source: { kind: "agent" as const, ref: "host" },
      name: "@digest",
      description: "digest",
      ui: [],
      requires_response: false,
      quick_replies: [],
      status: "done" as const,
      read: true,
      anchor: 0,
      ts: "2026-07-13T09:00:00Z",
    };
    store.dispatch({ type: "event_upsert", payload: { type: "event_upsert", event: doneEvent } });

    archiveEventWithUndo(12);
    const t = toast.toasts().find((x) => x.message === "Archived");
    t!.action!.onClick();

    // A finished card was never auto-dismissed, so its Undo unarchives only.
    expect(sent).toEqual([
      { action: "archive", eventId: 12, data: {} },
      { action: "unarchive", eventId: 12, data: {} },
    ]);
    expect(store.state.eventArchiveOverrides).toEqual([]);
  });

  it("the archive → host echo → resync flow leaves no override residue", async () => {
    const store = await import("../store/store");
    vi.doMock("../ws/client", () => ({
      getClient: () => ({ sendEventAction: () => {} }),
    }));
    const { archiveEventWithUndo } = await import("./event-archive");
    const event = {
      id: 9,
      kind: "summary" as const,
      source: { kind: "agent" as const, ref: "host" },
      name: "@digest",
      description: "digest",
      ui: [],
      requires_response: false,
      quick_replies: [],
      status: "done" as const,
      read: true,
      anchor: 0,
      ts: "2026-07-13T09:00:00Z",
    };
    store.dispatch({ type: "event_upsert", payload: { type: "event_upsert", event } });

    archiveEventWithUndo(9, { silent: true });
    expect(store.state.eventArchiveOverrides).toEqual([9]);

    // The host commits and broadcasts the archived flag back: the optimistic
    // layer is pruned, the wire truth carries the archive from here on.
    store.dispatch({
      type: "event_upsert",
      payload: { type: "event_upsert", event: { ...event, archived: true } },
    });
    expect(store.state.eventArchiveOverrides).toEqual([]);
    expect(store.state.events[0].archived).toBe(true);
  });
});
