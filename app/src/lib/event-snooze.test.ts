import { beforeEach, describe, expect, it, vi } from "vitest";

// Fresh store + mocked ws client per test (the shared singleton pattern,
// mirroring event-archive.test.ts).
beforeEach(() => {
  vi.resetModules();
});

const UNTIL = "2026-07-14T22:00:00Z";

describe("snoozeEventWithUndo — the event_action snooze round-trip", () => {
  it("optimistically sets snoozed_until and posts {until}, then removes it from Active", async () => {
    const store = await import("../store/store");
    const sent: { eventId: number; action: string; data: unknown }[] = [];
    vi.doMock("../ws/client", () => ({
      getClient: () => ({
        sendEventAction: (eventId: number, action: string, data: unknown) =>
          sent.push({ eventId, action, data }),
      }),
    }));
    const { snoozeEventWithUndo } = await import("./event-snooze");
    const { visibleEvents } = await import("../store/selectors");
    const event = {
      id: 5,
      kind: "judgment" as const,
      source: { kind: "agent" as const, ref: "host" },
      name: "@j",
      description: "d",
      ui: [],
      requires_response: true,
      quick_replies: [],
      status: "open" as const,
      read: false,
      anchor: 0,
      ts: "2026-07-14T09:00:00Z",
    };
    store.dispatch({ type: "event_upsert", payload: { type: "event_upsert", event } });

    const payload = snoozeEventWithUndo(5, UNTIL, "This evening", { silent: true });

    expect(payload).toEqual({
      type: "event_action",
      event_id: 5,
      action: "snooze",
      data: { until: UNTIL },
    });
    expect(sent).toEqual([{ eventId: 5, action: "snooze", data: { until: UNTIL } }]);
    // The wire truth is untouched; the assertion lives in the override record
    // and shows through the projection.
    expect(store.state.events[0].snoozed_until).toBeUndefined();
    expect(store.state.eventOverrides).toEqual({ 5: { snoozedUntil: UNTIL } });
    expect(store.effectiveEvents()[0].snoozed_until).toBe(UNTIL);
    // Future snoozed_until → excluded from the resting queue.
    const now = Date.parse("2026-07-14T10:00:00Z");
    expect(visibleEvents(store.effectiveEvents(), now)).toEqual([]);
  });

  it("raises a quiet Snoozed toast whose Undo un-snoozes (posts unsnooze, clears the field)", async () => {
    const store = await import("../store/store");
    const toast = await import("./toast");
    const sent: { eventId: number; action: string }[] = [];
    vi.doMock("../ws/client", () => ({
      getClient: () => ({
        sendEventAction: (eventId: number, action: string) => sent.push({ eventId, action }),
      }),
    }));
    const { snoozeEventWithUndo } = await import("./event-snooze");
    const event = {
      id: 8,
      kind: "judgment" as const,
      source: { kind: "agent" as const, ref: "host" },
      name: "@j",
      description: "d",
      ui: [],
      requires_response: true,
      quick_replies: [],
      status: "open" as const,
      read: false,
      anchor: 0,
      ts: "2026-07-14T09:00:00Z",
    };
    store.dispatch({ type: "event_upsert", payload: { type: "event_upsert", event } });

    snoozeEventWithUndo(8, UNTIL, "This evening");
    const t = toast.toasts().find((x) => /Snoozed/.test(x.message));
    expect(t?.action?.label).toBe("Undo");
    t!.action!.onClick();

    expect(sent).toEqual([
      { eventId: 8, action: "snooze" },
      { eventId: 8, action: "unsnooze" },
    ]);
    // Un-snooze asserts "no return instant" — the wire truth already says so,
    // so the assertion settles away and the event is simply back in Active.
    expect(store.state.eventOverrides).toEqual({});
    expect(store.effectiveEvents()[0].snoozed_until ?? null).toBeNull();
  });
});
