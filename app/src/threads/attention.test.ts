import { describe, expect, it } from "vitest";
import { attentionExcerpt, attentionQueue, attentionThreads, needsOwner } from "./attention";
import { emptyHistory, type ThreadHistory } from "./model";
import { makeThread } from "./fixtures";
const now = Date.parse("2026-09-09T12:00:00Z");
const history = (body: string): ThreadHistory => ({ ...emptyHistory(), loaded: true, messages: [
  { id: 1, thread_id: 1, author: "owner", body: "go", ref: null, ts: "2026-09-09T11:00:00Z" },
  { id: 2, thread_id: 1, author: "agent", body, ref: null, ts: "2026-09-09T11:30:00Z" },
] });
describe("one attention selector", () => {
  it("counts a waiting Thread but never an archived or still-sleeping one", () => {
    expect(needsOwner(makeThread(1, { attention: "needs_owner" }), now)).toBe(true);
    expect(needsOwner(makeThread(1, { attention: "needs_owner", archived_at: "2026-09-09T10:00:00Z" }), now)).toBe(false);
    expect(needsOwner(makeThread(1, { attention: "needs_owner", snoozed_until: "2026-09-09T13:00:00Z" }), now)).toBe(false);
    expect(needsOwner(makeThread(1, { attention: "needs_owner", snoozed_until: "2026-09-09T11:00:00Z" }), now)).toBe(true);
  });
  it("leads with the Thread that has waited longest", () => {
    const recent = makeThread(1, { attention: "needs_owner", last_activity_at: "2026-09-09T11:55:00Z" });
    const stale = makeThread(2, { attention: "needs_owner", last_activity_at: "2026-09-09T09:00:00Z" });
    expect(attentionThreads([recent, stale], now).map(thread => thread.id)).toEqual([2, 1]);
  });
  it("lifts the Agent's actual question out of its last message", () => {
    expect(attentionExcerpt("I looked at the logs.\n\n**Should I roll back to 1.4?**")).toBe("Should I roll back to 1.4?");
    expect(attentionExcerpt("Done — nothing else to report.")).toBe("Done — nothing else to report.");
    expect(attentionExcerpt("")).toBe("");
    expect(attentionExcerpt("x".repeat(400)).endsWith("…")).toBe(true);
  });
  it("orders waiting, then running, then recent, and never trims a waiting Thread", () => {
    const waiting = makeThread(1, { attention: "needs_owner", last_activity_at: "2026-09-09T09:00:00Z" });
    const running = makeThread(2, { running_turn: { requester_thread_id: null, requester_turn_id: null, id: 5, thread_id: 2, owner_message_id: null, agent_message_id: null, state: "running", accepted_at: "2026-09-09T11:00:00Z", started_at: "2026-09-09T11:00:00Z", finished_at: null }, last_activity_at: "2026-09-09T11:00:00Z" });
    const quiet = makeThread(3, { last_activity_at: "2026-09-09T11:50:00Z" });
    const done = makeThread(4, { kind: "task", settled_at: "2026-09-09T10:00:00Z" });
    const queue = attentionQueue([quiet, running, waiting, done], { 1: history("Which region first?") }, now, true, 1);
    expect(queue.map(entry => [entry.thread.id, entry.group])).toEqual([[1, "attention"]]);
    expect(queue[0].excerpt).toBe("Which region first?");
    expect(queue[0].waited).toBe("3h");
    const full = attentionQueue([quiet, running, waiting, done], {}, now, true);
    expect(full.map(entry => [entry.thread.id, entry.group])).toEqual([[1, "attention"], [2, "running"], [3, "recent"]]);
  });
});
