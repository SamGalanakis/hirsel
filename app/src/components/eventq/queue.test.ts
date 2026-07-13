import { describe, expect, it } from "vitest";
import { awarenessToAutoRead, nextOpenIndex } from "./queue";
import type { EventItem } from "../../protocol";

function ev(overrides: Partial<EventItem> = {}): EventItem {
  return {
    id: 1,
    kind: "judgment",
    source: { kind: "agent", ref: "host" },
    name: "@e",
    description: "d",
    ui: [],
    requires_response: true,
    quick_replies: [],
    status: "open",
    read: false,
    anchor: 0,
    ts: "2026-07-13T09:00:00Z",
    ...overrides,
  };
}

describe("nextOpenIndex", () => {
  it("skips resolved judgments and lands on the next open event", () => {
    const ordered = [ev({ id: 1 }), ev({ id: 2 }), ev({ id: 3 })];
    // From 0, event 2 is decided → advance past it to event 3 (index 2).
    expect(nextOpenIndex(ordered, 0, [2])).toBe(2);
    // From 0 with nothing decided → the immediate next (index 1).
    expect(nextOpenIndex(ordered, 0, [])).toBe(1);
  });

  it("lands on the clear page (length) when nothing open remains", () => {
    const ordered = [ev({ id: 1 }), ev({ id: 2 })];
    expect(nextOpenIndex(ordered, 0, [2])).toBe(2); // clear page index === length
  });

  it("does not skip awareness events (only resolved judgments)", () => {
    const ordered = [ev({ id: 1 }), ev({ id: 2, kind: "summary", requires_response: false })];
    expect(nextOpenIndex(ordered, 0, [])).toBe(1);
  });
});

describe("awarenessToAutoRead", () => {
  it("returns unread awareness strictly before the cursor, never judgments", () => {
    const ordered = [
      ev({ id: 1 }), // judgment
      ev({ id: 2, kind: "summary", requires_response: false, read: false }),
      ev({ id: 3, kind: "info", requires_response: false, read: true }),
      ev({ id: 4, kind: "info", requires_response: false, read: false }),
    ];
    // Cursor at index 3: events 0..2 are behind. Judgment (0) excluded; summary
    // (1) unread → included; info (2) already read → excluded.
    expect(awarenessToAutoRead(ordered, 3).map((e) => e.id)).toEqual([2]);
    // Nothing behind the first page.
    expect(awarenessToAutoRead(ordered, 0)).toEqual([]);
  });
});
