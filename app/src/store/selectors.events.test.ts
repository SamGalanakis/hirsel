import { describe, expect, it } from "vitest";
import {
  archivedEvents,
  eventTitle,
  eventUiNodes,
  finishedEvents,
  isEventArchived,
  isEventFinished,
  isEventResolved,
  isEventSnoozed,
  openJudgmentCount,
  orderedQueue,
  snoozedEvents,
  visibleEvents,
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

  it("sinks a decided judgment below open ones, awareness last (Wave-3: no session-snooze band)", () => {
    const decided = ev({ id: 1 });
    const open = ev({ id: 2 });
    const other = ev({ id: 3 });
    const summary = ev({ id: 4, kind: "summary", requires_response: false });
    const ordered = orderedQueue([decided, open, other, summary], [1]);
    // open (2, 3) first oldest-waited, decided judgment (1), awareness (4).
    expect(ordered.map((e) => e.id)).toEqual([2, 3, 1, 4]);
  });
});

describe("archive filter (contract v1): default-hides everywhere, counts stay honest", () => {
  it("isEventArchived folds the wire flag and the optimistic override together", () => {
    expect(isEventArchived(ev({ id: 1 }), [])).toBe(false);
    expect(isEventArchived(ev({ id: 1, archived: true }), [])).toBe(true);
    expect(isEventArchived(ev({ id: 1 }), [1])).toBe(true);
  });

  it("visibleEvents/archivedEvents partition the set; archived view is newest-first", () => {
    const live = ev({ id: 1 });
    const wireArchived = ev({ id: 2, status: "done", archived: true });
    const optimistic = ev({ id: 3, status: "done" });
    expect(visibleEvents([live, wireArchived, optimistic], [3]).map((e) => e.id)).toEqual([1]);
    expect(archivedEvents([live, wireArchived, optimistic], [3]).map((e) => e.id)).toEqual([3, 2]);
  });

  it("an archived OPEN judgment leaves the needs-you count (counts run on the filtered set)", () => {
    const a = ev({ id: 1 });
    const b = ev({ id: 2 });
    expect(openJudgmentCount(visibleEvents([a, b], []), [])).toBe(2);
    // The host archives an open judgment (agent `events.archive`): even before
    // its auto-dismiss lands, the filtered count no longer claims it needs you.
    expect(openJudgmentCount(visibleEvents([ev({ id: 1, archived: true }), b], []), [])).toBe(1);
    // Same via the optimistic layer.
    expect(openJudgmentCount(visibleEvents([a, b], [2]), [])).toBe(1);
  });

  it("isEventFinished matches the events.clear sweep: done OR (read AND not requires_response)", () => {
    expect(isEventFinished(ev({ id: 1 }), [])).toBe(false); // open judgment
    expect(isEventFinished(ev({ id: 1, status: "done" }), [])).toBe(true);
    expect(isEventFinished(ev({ id: 1 }), [1])).toBe(true); // optimistically decided
    const info = ev({ id: 2, kind: "info", requires_response: false });
    expect(isEventFinished(info, [])).toBe(false); // unread awareness
    expect(isEventFinished({ ...info, read: true }, [])).toBe(true);
  });
});

describe("durable snooze (Wave-3): leaves Active everywhere, counts stay honest", () => {
  const NOW = Date.parse("2026-07-14T12:00:00Z");
  const future = "2026-07-14T18:00:00Z";
  const past = "2026-07-14T06:00:00Z";

  it("isEventSnoozed is true only while snoozed_until is in the future", () => {
    expect(isEventSnoozed(ev({ snoozed_until: future }), NOW)).toBe(true);
    expect(isEventSnoozed(ev({ snoozed_until: past }), NOW)).toBe(false);
    expect(isEventSnoozed(ev({ snoozed_until: null }), NOW)).toBe(false);
    expect(isEventSnoozed(ev({}), NOW)).toBe(false);
  });

  it("visibleEvents excludes snoozed; snoozedEvents lists them soonest-return-first", () => {
    const live = ev({ id: 1 });
    const soon = ev({ id: 2, snoozed_until: "2026-07-14T14:00:00Z" });
    const later = ev({ id: 3, snoozed_until: "2026-07-14T20:00:00Z" });
    expect(visibleEvents([live, soon, later], [], NOW).map((e) => e.id)).toEqual([1]);
    expect(snoozedEvents([live, soon, later], [], NOW).map((e) => e.id)).toEqual([2, 3]);
  });

  it("a snoozed OPEN judgment leaves the needs-you count", () => {
    const a = ev({ id: 1 });
    const b = ev({ id: 2, snoozed_until: future });
    expect(openJudgmentCount(visibleEvents([a, b], [], NOW), [])).toBe(1);
  });

  it("finishedEvents is the sweep set: finished, not archived, not snoozed", () => {
    const openJudgment = ev({ id: 1 });
    const decided = ev({ id: 2, status: "done" });
    const readInfo = ev({ id: 3, kind: "info", requires_response: false, read: true });
    const snoozedDone = ev({ id: 4, status: "done", snoozed_until: future });
    const archivedDone = ev({ id: 5, status: "done", archived: true });
    const ids = finishedEvents([openJudgment, decided, readInfo, snoozedDone, archivedDone], [], [], NOW).map(
      (e) => e.id,
    );
    expect(ids.sort()).toEqual([2, 3]);
  });

  it("archivedEvents orders the day-log newest-first by archived_at (id fallback)", () => {
    const older = ev({ id: 1, archived: true, archived_at: "2026-07-12T09:00:00Z" });
    const newer = ev({ id: 2, archived: true, archived_at: "2026-07-13T09:00:00Z" });
    const noStamp = ev({ id: 3, archived: true }); // just-swept optimistic → top
    expect(archivedEvents([older, newer, noStamp], []).map((e) => e.id)).toEqual([3, 2, 1]);
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
