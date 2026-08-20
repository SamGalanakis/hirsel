import { describe, expect, it } from "vitest";
import { reduce } from "./reducer";
import { projectEvents } from "./selectors";
import { initialState } from "./types";
import { EventKind } from "../protocol";
import type { EventItem } from "../protocol";

function ev(overrides: Partial<EventItem> = {}): EventItem {
  return {
    id: 1,
    kind: EventKind.Judgment,
    source: { kind: "agent", ref: "hirsel-host" },
    name: "@fork",
    description: "a fork",
    ui: [{ type: "heading", text: "Which way?" }],
    requires_response: true,
    quick_replies: [],
    status: "open",
    read: false,
    anchor: 0,
    ts: "2026-07-13T09:00:00Z",
    ...overrides,
  };
}

describe("hello_ok seeds Tasks from typed Events", () => {
  it("seeds events from the frame and defaults to [] when absent", () => {
    const seeded = reduce(initialState(), {
      type: "hello_ok",
      payload: { type: "hello_ok", latest_msg_id: 0, messages: [], pings: [], events: [ev()] },
    });
    expect(seeded.events).toHaveLength(1);
    // A resync is authoritative: an event that vanished is gone; the ping slice
    // is untouched by the events field.
    const absent = reduce(seeded, {
      type: "hello_ok",
      payload: { type: "hello_ok", latest_msg_id: 0, messages: [], pings: [] },
    });
    expect(absent.events).toEqual([]);
  });

  it("drops assertions for events the snapshot no longer carries", () => {
    let s = reduce(initialState(), {
      type: "hello_ok",
      payload: { type: "hello_ok", latest_msg_id: 0, messages: [], pings: [], events: [ev({ id: 7 })] },
    });
    s = reduce(s, { type: "event_decide_local", eventId: 7 });
    expect(s.eventOverrides).toEqual({ 7: { decided: true } });
    // Resync without event 7: the stale assertion is dropped.
    s = reduce(s, {
      type: "hello_ok",
      payload: { type: "hello_ok", latest_msg_id: 0, messages: [], pings: [], events: [] },
    });
    expect(s.eventOverrides).toEqual({});
  });

  it("settles every assertion the snapshot has caught up with, and only those", () => {
    let s = reduce(initialState(), {
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: 0,
        messages: [],
        pings: [],
        events: [
          ev({ id: 1 }),
          ev({ id: 2, kind: EventKind.Info, requires_response: false }),
          ev({ id: 3 }),
          ev({ id: 4 }),
        ],
      },
    });
    s = reduce(s, { type: "event_decide_local", eventId: 1 });
    s = reduce(s, { type: "event_read_local", eventId: 2 });
    s = reduce(s, { type: "event_snooze_local", eventId: 3, until: "2026-07-20T18:00:00Z" });
    s = reduce(s, { type: "event_archive_local", eventId: 4 });
    expect(s.eventOverrides).toEqual({
      1: { decided: true },
      2: { read: true },
      3: { snoozedUntil: "2026-07-20T18:00:00Z" },
      4: { archived: true },
    });

    // Resync: the host committed the decide and the read (the same instant,
    // spelled with an offset rather than Z, still settles the snooze), but the
    // archive never reached it — so ONLY that assertion survives.
    s = reduce(s, {
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: 0,
        messages: [],
        pings: [],
        events: [
          ev({ id: 1, status: "done" }),
          ev({ id: 2, kind: EventKind.Info, requires_response: false, read: true }),
          ev({ id: 3, snoozed_until: "2026-07-20T18:00:00+00:00" }),
          ev({ id: 4 }),
        ],
      },
    });
    expect(s.eventOverrides).toEqual({ 4: { archived: true } });
  });
});

describe("event_upsert", () => {
  it("inserts a new event and updates an existing one in place", () => {
    let s = reduce(initialState(), {
      type: "event_upsert",
      payload: { type: "event_upsert", event: ev({ id: 1 }) },
    });
    s = reduce(s, { type: "event_upsert", payload: { type: "event_upsert", event: ev({ id: 2, kind: EventKind.Info }) } });
    expect(s.events.map((e) => e.id)).toEqual([1, 2]);
    s = reduce(s, {
      type: "event_upsert",
      payload: { type: "event_upsert", event: ev({ id: 1, description: "updated" }) },
    });
    expect(s.events).toHaveLength(2);
    expect(s.events[0].description).toBe("updated");
  });

  it("prunes the optimistic decide override once the host commits a done event", () => {
    let s = reduce(initialState(), {
      type: "event_upsert",
      payload: { type: "event_upsert", event: ev({ id: 3 }) },
    });
    s = reduce(s, { type: "event_decide_local", eventId: 3 });
    expect(s.eventOverrides).toEqual({ 3: { decided: true } });
    // The host's done upsert supersedes the optimistic layer.
    s = reduce(s, {
      type: "event_upsert",
      payload: { type: "event_upsert", event: ev({ id: 3, status: "done" }) },
    });
    expect(s.eventOverrides).toEqual({});
    expect(s.events[0].status).toBe("done");
  });
});

describe("optimistic decide / undecide / read", () => {
  it("decide_local records once (idempotent), undecide drops it", () => {
    let s = reduce(initialState(), { type: "event_decide_local", eventId: 5 });
    s = reduce(s, { type: "event_decide_local", eventId: 5 });
    expect(s.eventOverrides).toEqual({ 5: { decided: true } });
    s = reduce(s, { type: "event_undecide_local", eventId: 5 });
    expect(s.eventOverrides).toEqual({});
  });

  it("event_read_local asserts read on the matching event only, leaving the wire truth alone", () => {
    let s = reduce(initialState(), {
      type: "event_upsert",
      payload: { type: "event_upsert", event: ev({ id: 9, kind: EventKind.Summary, read: false }) },
    });
    s = reduce(s, { type: "event_read_local", eventId: 9 });
    expect(s.eventOverrides).toEqual({ 9: { read: true } });
    expect(s.events[0].read).toBe(false);
    expect(projectEvents(s.events, s.eventOverrides)[0].read).toBe(true);
  });

  it("a gesture that only restates the wire truth leaves no residue", () => {
    let s = reduce(initialState(), {
      type: "event_upsert",
      payload: { type: "event_upsert", event: ev({ id: 9, kind: EventKind.Summary, read: true }) },
    });
    s = reduce(s, { type: "event_read_local", eventId: 9 });
    expect(s.eventOverrides).toEqual({});
  });
});

describe("optimistic archive / unarchive (archive contract v1)", () => {
  it("archive_local records once (idempotent); unarchive_local drops it", () => {
    let s = reduce(initialState(), { type: "event_archive_local", eventId: 4 });
    s = reduce(s, { type: "event_archive_local", eventId: 4 });
    expect(s.eventOverrides).toEqual({ 4: { archived: true } });
    s = reduce(s, { type: "event_unarchive_local", eventId: 4 });
    expect(s.eventOverrides).toEqual({});
  });

  it("unarchive_local asserts not-archived over a WIRE-archived event (host echo settles it)", () => {
    let s = reduce(initialState(), {
      type: "event_upsert",
      payload: { type: "event_upsert", event: ev({ id: 6, status: "done", archived: true }) },
    });
    s = reduce(s, { type: "event_unarchive_local", eventId: 6 });
    expect(s.eventOverrides).toEqual({ 6: { archived: false } });
    // The wire truth is left intact; the projection is what the surfaces read.
    expect(s.events[0].archived).toBe(true);
    expect(projectEvents(s.events, s.eventOverrides)[0].archived).toBe(false);
  });

  it("a committed archived event_upsert prunes the optimistic override; an unrelated upsert never does", () => {
    let s = reduce(initialState(), {
      type: "event_upsert",
      payload: { type: "event_upsert", event: ev({ id: 8, status: "done" }) },
    });
    s = reduce(s, { type: "event_archive_local", eventId: 8 });
    // An interleaved upsert that still carries archived=false (e.g. a read-flip
    // broadcast racing the archive echo) must NOT flicker the card back.
    s = reduce(s, {
      type: "event_upsert",
      payload: { type: "event_upsert", event: ev({ id: 8, status: "done", read: true }) },
    });
    expect(s.eventOverrides).toEqual({ 8: { archived: true } });
    // The committed archived truth supersedes the optimistic layer.
    s = reduce(s, {
      type: "event_upsert",
      payload: { type: "event_upsert", event: ev({ id: 8, status: "done", archived: true }) },
    });
    expect(s.eventOverrides).toEqual({});
    expect(s.events[0].archived).toBe(true);
  });

  it("hello_ok prunes archive overrides for vanished or already-archived events", () => {
    let s = reduce(initialState(), {
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: 0,
        messages: [],
        pings: [],
        events: [ev({ id: 1 }), ev({ id: 2 }), ev({ id: 3 })],
      },
    });
    s = reduce(s, { type: "event_archive_local", eventId: 1 });
    s = reduce(s, { type: "event_archive_local", eventId: 2 });
    s = reduce(s, { type: "event_archive_local", eventId: 3 });
    // Resync: 1 vanished, 2 now committed archived, 3 still live un-archived
    // (the host never saw the action — the override keeps it swept client-side).
    s = reduce(s, {
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: 0,
        messages: [],
        pings: [],
        events: [ev({ id: 2, status: "done", archived: true }), ev({ id: 3 })],
      },
    });
    expect(s.eventOverrides).toEqual({ 3: { archived: true } });
  });
});
