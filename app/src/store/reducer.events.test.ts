import { describe, expect, it } from "vitest";
import { reduce } from "./reducer";
import { initialState } from "./types";
import type { EventItem } from "../protocol";

function ev(overrides: Partial<EventItem> = {}): EventItem {
  return {
    id: 1,
    kind: "judgment",
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

describe("hello_ok seeds the event queue (generalizing pings)", () => {
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

  it("prunes decide overrides for events the snapshot no longer carries", () => {
    let s = reduce(initialState(), {
      type: "hello_ok",
      payload: { type: "hello_ok", latest_msg_id: 0, messages: [], pings: [], events: [ev({ id: 7 })] },
    });
    s = reduce(s, { type: "event_decide_local", eventId: 7 });
    expect(s.eventDecideOverrides).toEqual([7]);
    // Resync without event 7: the stale override is dropped.
    s = reduce(s, {
      type: "hello_ok",
      payload: { type: "hello_ok", latest_msg_id: 0, messages: [], pings: [], events: [] },
    });
    expect(s.eventDecideOverrides).toEqual([]);
  });
});

describe("event_upsert", () => {
  it("inserts a new event and updates an existing one in place", () => {
    let s = reduce(initialState(), {
      type: "event_upsert",
      payload: { type: "event_upsert", event: ev({ id: 1 }) },
    });
    s = reduce(s, { type: "event_upsert", payload: { type: "event_upsert", event: ev({ id: 2, kind: "info" }) } });
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
    expect(s.eventDecideOverrides).toEqual([3]);
    // The host's done upsert supersedes the optimistic layer.
    s = reduce(s, {
      type: "event_upsert",
      payload: { type: "event_upsert", event: ev({ id: 3, status: "done" }) },
    });
    expect(s.eventDecideOverrides).toEqual([]);
    expect(s.events[0].status).toBe("done");
  });
});

describe("optimistic decide / undecide / read", () => {
  it("decide_local records once (idempotent), undecide drops it", () => {
    let s = reduce(initialState(), { type: "event_decide_local", eventId: 5 });
    s = reduce(s, { type: "event_decide_local", eventId: 5 });
    expect(s.eventDecideOverrides).toEqual([5]);
    s = reduce(s, { type: "event_undecide_local", eventId: 5 });
    expect(s.eventDecideOverrides).toEqual([]);
  });

  it("read_local flips read=true on the matching event only", () => {
    let s = reduce(initialState(), {
      type: "event_upsert",
      payload: { type: "event_upsert", event: ev({ id: 9, kind: "summary", read: false }) },
    });
    s = reduce(s, { type: "event_read_local", eventId: 9 });
    expect(s.events[0].read).toBe(true);
  });
});

describe("optimistic archive / unarchive (archive contract v1)", () => {
  it("archive_local records once (idempotent); unarchive_local drops it", () => {
    let s = reduce(initialState(), { type: "event_archive_local", eventId: 4 });
    s = reduce(s, { type: "event_archive_local", eventId: 4 });
    expect(s.eventArchiveOverrides).toEqual([4]);
    s = reduce(s, { type: "event_unarchive_local", eventId: 4 });
    expect(s.eventArchiveOverrides).toEqual([]);
  });

  it("unarchive_local also flips a wire-archived event's local flag (host echo reconciles)", () => {
    let s = reduce(initialState(), {
      type: "event_upsert",
      payload: { type: "event_upsert", event: ev({ id: 6, status: "done", archived: true }) },
    });
    s = reduce(s, { type: "event_unarchive_local", eventId: 6 });
    expect(s.events[0].archived).toBe(false);
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
    expect(s.eventArchiveOverrides).toEqual([8]);
    // The committed archived truth supersedes the optimistic layer.
    s = reduce(s, {
      type: "event_upsert",
      payload: { type: "event_upsert", event: ev({ id: 8, status: "done", archived: true }) },
    });
    expect(s.eventArchiveOverrides).toEqual([]);
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
    expect(s.eventArchiveOverrides).toEqual([3]);
  });
});
