import { describe, expect, it } from "vitest";
import { firstOpenIndex, nextOpenIndex, shouldMarkReadOnLeave } from "./queue";
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

describe("firstOpenIndex", () => {
  it("anchors to the first still-open judgment, else 0", () => {
    const ordered = [
      ev({ id: 1, kind: "summary", requires_response: false }),
      ev({ id: 2 }),
      ev({ id: 3 }),
    ];
    // Event 2 is the first open judgment.
    expect(firstOpenIndex(ordered, [])).toBe(1);
    // Event 2 optimistically decided → the next open one.
    expect(firstOpenIndex(ordered, [2])).toBe(2);
    // Nothing open → the top of the queue, never a wild offset.
    expect(firstOpenIndex(ordered, [2, 3])).toBe(0);
    expect(firstOpenIndex([], [])).toBe(0);
  });
});

describe("shouldMarkReadOnLeave", () => {
  it("marks only unread awareness — never judgments, read cards, or missing events", () => {
    expect(shouldMarkReadOnLeave(ev({ kind: "summary", requires_response: false, read: false }))).toBe(true);
    expect(shouldMarkReadOnLeave(ev({ kind: "info", requires_response: false, read: false }))).toBe(true);
    // A judgment is decided, not "read".
    expect(shouldMarkReadOnLeave(ev({ kind: "judgment" }))).toBe(false);
    // Already read → nothing to do.
    expect(shouldMarkReadOnLeave(ev({ kind: "summary", requires_response: false, read: true }))).toBe(false);
    // The viewed event vanished in a snapshot swap → never marks.
    expect(shouldMarkReadOnLeave(undefined)).toBe(false);
  });
});
