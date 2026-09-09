import { describe, expect, it } from "vitest";
import { elapsedTime, threadStatus } from "./status";
import { makeThread } from "./fixtures";
import type { ThreadTurn } from "./types";
const now = Date.parse("2026-09-09T12:12:00Z");
const turn: ThreadTurn = { id: 3, thread_id: 1, owner_message_id: 4, agent_message_id: null, state: "running", started_at: "2026-09-09T12:00:00Z", finished_at: null };
describe("authoritative Thread row status", () => {
  it("shows actual running duration and coexisting queue independently of settlement/attention/read", () => {
    const thread = makeThread(1, { running_turn: turn, queued_turn_count: 2, attention: "needs_owner", read: false, settled_at: "2026-09-09T11:00:00Z", updated_at: "2026-09-09T12:11:59Z" });
    expect(threadStatus(thread, now, true)).toMatchObject({ label: "Working 12m · 2 queued", state: "running", timestamp: turn.started_at });
    expect(thread.settled_at).not.toBeNull(); expect(thread.read).toBe(false);
  });
  it("keeps queued work separate from running duration", () => {
    expect(threadStatus(makeThread(1, { queued_turn_count: 2 }), now, true)).toMatchObject({ label: "Queued (2)", age: null });
  });
  it.each([["completed", "Turn finished"], ["failed", "Turn failed"], ["interrupted", "Interrupted"], ["cancelled", "Cancelled"]] as const)("preserves %s outcome without settling the Thread", (state, label) => {
    const thread = makeThread(1, { last_finished_turn: { ...turn, state, finished_at: "2026-09-09T12:07:00Z" } });
    expect(threadStatus(thread, now, true)).toMatchObject({ label, age: "5m" });
    expect(thread.settled_at).toBeNull();
  });
  it("does not advance activity recency for a read or lifecycle update", () => {
    const thread = makeThread(1, { last_activity_at: "2026-09-09T12:00:00Z", updated_at: "2026-09-09T12:11:59Z", read: true });
    expect(threadStatus(thread, now, true)).toMatchObject({ age: "12m", timestamp: thread.last_activity_at });
  });
  it("labels offline data without a fresh running duration", () => {
    expect(threadStatus(makeThread(1, { running_turn: turn }), now, false)).toMatchObject({ label: "Last known: Working", age: null, stale: true });
  });
  it("rejects invalid or far-future anchors and clamps small clock skew", () => {
    expect(elapsedTime("invalid", now)).toBeNull();
    expect(elapsedTime("2026-09-09T12:15:00Z", now)).toBeNull();
    expect(elapsedTime("2026-09-09T12:12:20Z", now)).toBe("<1m");
  });
});
