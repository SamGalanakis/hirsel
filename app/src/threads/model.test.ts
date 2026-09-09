import { describe, expect, it } from "vitest";
import { emptyHistory, mergeDetail, threadSection, upsertThread } from "./model";
import type { ThreadDetail } from "./types";
import { makeThread } from "./fixtures";
describe("durable thread inventory", () => {
  it("shows ordinary quiet work immediately, independently of read and attention", () => {
    let rows = upsertThread([], makeThread());
    expect(rows.map(t => t.id)).toEqual([1]);
    rows = upsertThread(rows, makeThread(1, { read: true, revision: 2 }));
    expect(threadSection(rows[0])).toBe("active");
    expect(threadSection(makeThread(1, { attention: "needs_owner" }))).toBe("active");
  });
  it("rejects older lifecycle revisions but accepts equal-revision activity updates", () => {
    const rows = [makeThread(1, { title: "Current", revision: 3 })];
    expect(upsertThread(rows, makeThread())).toBe(rows);
    expect(upsertThread(rows, makeThread(1, { title: "Current", revision: 3, queued_turn_count: 2 }))[0].queued_turn_count).toBe(2);
  });
  it("keeps settlement explicit and snooze time-bound", () => {
    const now = Date.parse("2026-09-09T10:00:00Z");
    expect(threadSection(makeThread(1, { read: true }), now)).toBe("active");
    expect(threadSection(makeThread(1, { settled_at: "2026-09-09T09:00:00Z" }), now)).toBe("settled");
    expect(threadSection(makeThread(1, { snoozed_until: "2026-09-09T11:00:00Z" }), now)).toBe("snoozed");
    expect(threadSection(makeThread(1, { snoozed_until: "2026-09-09T09:00:00Z" }), now)).toBe("active");
  });
  it("merges replay and history pages without losing live messages or cross-owning citations", () => {
    const row = (id: number, thread_id: number, mentions: number[] = []) => ({ id, thread_id, mentions, author: "owner" as const, body: "same", ref: null, ts: "2026-09-09T10:00:00Z" });
    const detail: ThreadDetail = { thread: makeThread(), messages: [row(1, 1), row(2, 2, [1])], turns: [], activities: [], has_more: false };
    const prior = { ...emptyHistory(), messages: [row(3, 1)] };
    const merged = mergeDetail(prior, detail, false);
    expect(merged.messages.map(m => m.id)).toEqual([1, 3]);
    expect(mergeDetail(merged, detail, false).messages).toEqual(merged.messages);
  });
});
