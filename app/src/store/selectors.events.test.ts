import { describe, expect, it } from "vitest";
import {
  eventTitle,
  eventUiNodes,
  finishedEvents,
  isEventArchived,
  isEventFinished,
  isEventResolved,
  isEventSnoozed,
  orderedTasks,
  taskEvents,
  tasksNeedingOwnerCount,
  visibleEvents,
} from "./selectors";
import { mostNeedingTask } from "../components/tasks/task-model";
import { EventKind } from "../protocol";
import type { EventItem, ViewSpec } from "../protocol";

function ev(overrides: Partial<EventItem> = {}): EventItem {
  return {
    id: 1,
    kind: EventKind.Judgment,
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

describe("isEventResolved / tasksNeedingOwnerCount", () => {
  it("reads the projected status and counts only open judgments", () => {
    const a = ev({ id: 1 });
    const b = ev({ id: 2 });
    const info = ev({ id: 3, kind: EventKind.Info, requires_response: false });
    expect(isEventResolved(a)).toBe(false);
    expect(isEventResolved(ev({ id: 4, status: "done" }))).toBe(true);
    // Only open judgments contribute to the ONE red — info never does.
    expect(tasksNeedingOwnerCount([a, b, info])).toBe(2);
    // An optimistically-decided event arrives here already projected to done.
    expect(tasksNeedingOwnerCount([{ ...a, status: "done" }, b, info])).toBe(1);
  });
});

describe("orderedTasks priority", () => {
  it("orders blocking judgment → needs-you → decided → awareness, oldest-waited first", () => {
    const blocking = ev({ id: 10, blocking: true, ts: "2026-07-13T09:30:00Z" });
    const olderJudgment = ev({ id: 11, ts: "2026-07-13T08:00:00Z" });
    const newerJudgment = ev({ id: 12, ts: "2026-07-13T09:00:00Z" });
    const summary = ev({ id: 13, kind: EventKind.Summary, requires_response: false, ts: "2026-07-13T06:00:00Z" });
    const info = ev({ id: 14, kind: EventKind.Info, requires_response: false, ts: "2026-07-13T07:00:00Z" });
    const ordered = orderedTasks([summary, newerJudgment, info, blocking, olderJudgment]);
    // Blocking first, then the two open judgments oldest-first, then awareness.
    expect(ordered.map((e) => e.id)).toEqual([10, 11, 12, 13, 14]);
  });

  it("sinks a decided judgment below open ones, awareness last (Wave-3: no session-snooze band)", () => {
    const decided = ev({ id: 1, status: "done" });
    const open = ev({ id: 2 });
    const other = ev({ id: 3 });
    const summary = ev({ id: 4, kind: EventKind.Summary, requires_response: false });
    const ordered = orderedTasks([decided, open, other, summary]);
    // open (2, 3) first oldest-waited, decided judgment (1), awareness (4).
    expect(ordered.map((e) => e.id)).toEqual([2, 3, 1, 4]);
  });
});

describe("archive filter (contract v1): default-hides everywhere, counts stay honest", () => {
  it("isEventArchived reads the projected archived flag", () => {
    expect(isEventArchived(ev({ id: 1 }))).toBe(false);
    expect(isEventArchived(ev({ id: 1, archived: true }))).toBe(true);
  });

  it("visibleEvents excludes archived events (wire flag or projected override)", () => {
    const live = ev({ id: 1 });
    const wireArchived = ev({ id: 2, status: "done", archived: true });
    // An optimistically-archived event reaches the selector already projected.
    const optimistic = ev({ id: 3, status: "done", archived: true });
    expect(visibleEvents([live, wireArchived, optimistic]).map((e) => e.id)).toEqual([1]);
  });

  it("an archived OPEN judgment leaves the needs-you count (counts run on the filtered set)", () => {
    const a = ev({ id: 1 });
    const b = ev({ id: 2 });
    expect(tasksNeedingOwnerCount(visibleEvents([a, b]))).toBe(2);
    // The host archives an open judgment (agent `events.archive`): even before
    // its auto-dismiss lands, the filtered count no longer claims it needs you.
    expect(tasksNeedingOwnerCount(visibleEvents([ev({ id: 1, archived: true }), b]))).toBe(1);
    // Same via the optimistic layer, which projects onto the very same flag.
    expect(tasksNeedingOwnerCount(visibleEvents([a, { ...b, archived: true }]))).toBe(1);
  });

  it("isEventFinished matches the events.clear sweep: done OR (read AND not requires_response)", () => {
    expect(isEventFinished(ev({ id: 1 }))).toBe(false); // open judgment
    expect(isEventFinished(ev({ id: 1, status: "done" }))).toBe(true);
    const info = ev({ id: 2, kind: EventKind.Info, requires_response: false });
    expect(isEventFinished(info)).toBe(false); // unread awareness
    expect(isEventFinished({ ...info, read: true })).toBe(true);
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

  it("visibleEvents excludes snoozed events", () => {
    const live = ev({ id: 1 });
    const soon = ev({ id: 2, snoozed_until: "2026-07-14T14:00:00Z" });
    const later = ev({ id: 3, snoozed_until: "2026-07-14T20:00:00Z" });
    expect(visibleEvents([live, soon, later], NOW).map((e) => e.id)).toEqual([1]);
  });

  it("a snoozed OPEN judgment leaves the needs-you count", () => {
    const a = ev({ id: 1 });
    const b = ev({ id: 2, snoozed_until: future });
    expect(tasksNeedingOwnerCount(visibleEvents([a, b], NOW))).toBe(1);
  });

  it("finishedEvents is the sweep set: finished, not archived, not snoozed, not info", () => {
    const openJudgment = ev({ id: 1 });
    const decided = ev({ id: 2, status: "done" });
    const readSummary = ev({ id: 3, kind: EventKind.Summary, requires_response: false, read: true });
    const snoozedDone = ev({ id: 4, status: "done", snoozed_until: future });
    const archivedDone = ev({ id: 5, status: "done", archived: true });
    // Info was never shown as a chip, so the sweep never claims to remove it —
    // "Clear finished (n)" counts exactly the chips that disappear.
    const readInfo = ev({ id: 6, kind: EventKind.Info, requires_response: false, read: true });
    const ids = finishedEvents(
      [openJudgment, decided, readSummary, snoozedDone, archivedDone, readInfo],
      NOW,
    ).map((e) => e.id);
    expect(ids.sort()).toEqual([2, 3]);
  });
});

describe("taskEvents: housekeeping info is never a Task", () => {
  it("drops info from the resting queue while visibleEvents keeps it", () => {
    const judgment = ev({ id: 1 });
    const summary = ev({ id: 2, kind: EventKind.Summary, requires_response: false });
    const info = ev({ id: 3, kind: EventKind.Info, requires_response: false });
    expect(visibleEvents([judgment, summary, info]).map((e) => e.id)).toEqual([1, 2, 3]);
    expect(taskEvents([judgment, summary, info]).map((e) => e.id)).toEqual([1, 2]);
  });

  it("keeps the lifecycle filters: an archived or snoozed Task is out too", () => {
    const NOW = Date.parse("2026-07-14T12:00:00Z");
    const live = ev({ id: 1 });
    const archived = ev({ id: 2, archived: true });
    const snoozed = ev({ id: 3, snoozed_until: "2026-07-14T18:00:00Z" });
    const info = ev({ id: 4, kind: EventKind.Info, requires_response: false });
    expect(taskEvents([live, archived, snoozed, info], NOW).map((e) => e.id)).toEqual([1]);
  });

  it("never counts info toward needs-you, and never lets it win auto-focus", () => {
    // Info carries no judgment, so the red count is unmoved either way — the
    // point is that the count and the rail read the SAME set.
    const info = ev({ id: 1, kind: EventKind.Info, requires_response: false });
    const judgment = ev({ id: 2 });
    expect(tasksNeedingOwnerCount(taskEvents([info, judgment]))).toBe(1);
    // Alone in the queue, an info event leaves the field ambient rather than
    // opening a chip nobody asked for.
    expect(mostNeedingTask(taskEvents([info]))).toBeNull();
    expect(mostNeedingTask(taskEvents([info, judgment]))?.id).toBe(2);
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
