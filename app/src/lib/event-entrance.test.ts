import { beforeEach, describe, expect, it, vi } from "vitest";
import type { EventItem } from "../protocol";

// Each test runs against a pristine copy of the store + entrance singletons:
// resetModules + dynamic import means the store's `dispatch` and the
// `event-entrance` module it marks arrivals in are the same fresh instance the
// test reads back.
beforeEach(() => {
  vi.resetModules();
});

function ev(id: number, overrides: Partial<EventItem> = {}): EventItem {
  return {
    id,
    kind: "judgment",
    source: { kind: "agent", ref: "hirsel-host" },
    name: `@e${id}`,
    description: "a judgment",
    ui: [{ type: "heading", text: "Which way?" }],
    requires_response: true,
    quick_replies: [],
    status: "open",
    read: false,
    anchor: id,
    ts: "2026-07-13T09:00:00Z",
    ...overrides,
  };
}

describe("event-entrance — the genuine-arrival flag", () => {
  it("consumeArrival is true exactly once for a marked id, false otherwise", async () => {
    const { markArrival, consumeArrival } = await import("./event-entrance");
    // Unmarked: never animates.
    expect(consumeArrival(1)).toBe(false);
    markArrival(1);
    // Consumed once…
    expect(consumeArrival(1)).toBe(true);
    // …and never again (a re-render must not replay the entrance).
    expect(consumeArrival(1)).toBe(false);
  });
});

describe("event-entrance — wired through the store", () => {
  it("marks a genuinely new event_upsert as an arrival, but never a hello_ok snapshot", async () => {
    const store = await import("../store/store");
    const { consumeArrival } = await import("./event-entrance");

    // Initial hydration of two events: a snapshot, NOT arrivals — the queue must
    // not flash every card in on load.
    store.dispatch({
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: 0,
        messages: [],
        pings: [],
        events: [ev(1), ev(2)],
      },
    });
    expect(consumeArrival(1)).toBe(false);
    expect(consumeArrival(2)).toBe(false);

    // A live event_upsert introducing a NEW id: a genuine arrival → animates.
    store.dispatch({
      type: "event_upsert",
      payload: { type: "event_upsert", event: ev(3) },
    });
    expect(consumeArrival(3)).toBe(true);
    // consumed — one entrance only.
    expect(consumeArrival(3)).toBe(false);
  });

  it("does not re-mark an event_upsert for an id already in the queue", async () => {
    const store = await import("../store/store");
    const { consumeArrival } = await import("./event-entrance");

    store.dispatch({
      type: "hello_ok",
      payload: { type: "hello_ok", latest_msg_id: 0, messages: [], pings: [], events: [ev(1)] },
    });
    // A re-upsert of an existing event (a read/decide echo) is not an arrival.
    store.dispatch({
      type: "event_upsert",
      payload: { type: "event_upsert", event: ev(1, { read: true }) },
    });
    expect(consumeArrival(1)).toBe(false);
  });
});
