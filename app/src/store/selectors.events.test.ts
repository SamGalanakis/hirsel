import { describe, expect, it } from "vitest";
import {
  eventTitle,
  eventUiNodes,
  isEventResolved,
  openJudgmentCount,
  orderedQueue,
} from "./selectors";
import type { EventItem, ViewSpec } from "../protocol";

function ev(overrides: Partial<EventItem> = {}): EventItem {
  return {
    id: 1,
    kind: "judgment",
    source: { kind: "agent", ref: "host" },
    name: "@e",
    description: "desc",
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

describe("isEventResolved / openJudgmentCount", () => {
  it("folds in the optimistic decide override and counts only open judgments", () => {
    const a = ev({ id: 1 });
    const b = ev({ id: 2 });
    const info = ev({ id: 3, kind: "info", requires_response: false });
    expect(isEventResolved(a, [])).toBe(false);
    expect(isEventResolved(a, [1])).toBe(true);
    expect(isEventResolved(ev({ id: 4, status: "done" }), [])).toBe(true);
    // Only open judgments contribute to the ONE red — info never does.
    expect(openJudgmentCount([a, b, info], [])).toBe(2);
    expect(openJudgmentCount([a, b, info], [1])).toBe(1);
  });
});

describe("orderedQueue priority (ADR-0012 interrupt-vs-accrue)", () => {
  it("orders blocking judgment → needs-you → decided → awareness, oldest-waited first", () => {
    const blocking = ev({ id: 10, blocking: true, ts: "2026-07-13T09:30:00Z" });
    const olderJudgment = ev({ id: 11, ts: "2026-07-13T08:00:00Z" });
    const newerJudgment = ev({ id: 12, ts: "2026-07-13T09:00:00Z" });
    const summary = ev({ id: 13, kind: "summary", requires_response: false, ts: "2026-07-13T06:00:00Z" });
    const info = ev({ id: 14, kind: "info", requires_response: false, ts: "2026-07-13T07:00:00Z" });
    const ordered = orderedQueue([summary, newerJudgment, info, blocking, olderJudgment], []);
    // Blocking first, then the two open judgments oldest-first, then awareness.
    expect(ordered.map((e) => e.id)).toEqual([10, 11, 12, 13, 14]);
  });

  it("sinks a decided judgment below open ones and a snoozed one to the tail band", () => {
    const decided = ev({ id: 1 });
    const open = ev({ id: 2 });
    const snoozedEv = ev({ id: 3 });
    const summary = ev({ id: 4, kind: "summary", requires_response: false });
    const ordered = orderedQueue([decided, open, snoozedEv, summary], [1], new Set([3]));
    // open (2) first, snoozed (3), decided judgment (1), awareness (4).
    expect(ordered.map((e) => e.id)).toEqual([2, 3, 1, 4]);
  });
});

describe("eventUiNodes / eventTitle", () => {
  it("unwraps a card root, passes an array through, and wraps a lone node", () => {
    const card: ViewSpec = { type: "card", children: [{ type: "heading", text: "H" }, { type: "text", text: "t" }] };
    expect(eventUiNodes(card).map((n) => n.type)).toEqual(["heading", "text"]);
    expect(eventUiNodes([{ type: "status", label: "ok" }]).map((n) => n.type)).toEqual(["status"]);
    expect(eventUiNodes({ type: "text", text: "solo" }).map((n) => n.type)).toEqual(["text"]);
    expect(eventUiNodes(undefined)).toEqual([]);
  });

  it("derives a title from the heading (backticks stripped), else status/text/name", () => {
    expect(eventTitle(ev({ ui: [{ type: "heading", text: "Wire `reopen`?" }] }))).toBe("Wire reopen?");
    expect(eventTitle(ev({ ui: [{ type: "status", label: "CI green" }] }))).toBe("CI green");
    expect(eventTitle(ev({ ui: [], description: "fallback desc" }))).toBe("fallback desc");
  });
});
