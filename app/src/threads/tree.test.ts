import { describe, expect, it } from "vitest";
import { makeThread } from "./fixtures";
import { threadAncestors, threadPath, threadTree } from "./tree";
const now = Date.parse("2026-09-10T12:00:00Z");
describe("human Thread hierarchy", () => {
  const threads = [makeThread(0, { title: "General" }), makeThread(4, { title: "Website", pinned_at: "2026-09-09T10:00:00Z" }), makeThread(7, { title: "Review", parent_thread_id: 4 }), makeThread(8, { title: "Review", parent_thread_id: 7 })];
  it("keeps stable numeric identity and full ancestry for duplicate titles and retained zero", () => {
    expect(threadAncestors(threads, 8).map(thread => thread.id)).toEqual([4, 7]);
    expect(threadPath(threads, 8)).toBe("Website #4 / Review #7 / Review #8");
    expect(threadTree(threads, "active", now, new Set([4,7])).map(row => [row.thread.id, row.depth])).toEqual([[4,0],[7,1],[8,2],[0,0]]);
    expect(threadTree(threads, "active", now, new Set()).map(row => row.thread.id)).toEqual([4,0]);
  });
  it("puts pinned roots first once with their children while ignoring legacy child pins", () => {
    const rows = [makeThread(0), makeThread(4, { pinned_at: "2026-09-09T10:00:00Z" }), makeThread(5, { parent_thread_id: 4 }), makeThread(6, { parent_thread_id: 4, pinned_at: "2026-09-01T10:00:00Z" }), makeThread(9, { pinned_at: "2026-09-08T10:00:00Z" })];
    expect(threadTree(rows, "active", now, new Set([4])).map(row => [row.thread.id, row.depth])).toEqual([[9,0],[4,0],[5,1],[6,1],[0,0]]);
    expect(rows[3].pinned_at).not.toBeNull();
  });
  it("does not pull hidden pinned roots into another lifecycle filter", () => {
    const rows = [makeThread(1, { pinned_at: "2026-09-10T10:00:00Z", archived_at: "2026-09-10T11:00:00Z" }), makeThread(2)];
    expect(threadTree(rows, "active", now, new Set()).map(row => row.thread.id)).toEqual([2]);
    expect(threadTree(rows, "archived", now, new Set()).map(row => row.thread.id)).toEqual([1]);
  });
  it("orders root pins at timestamp precision with ID ties", () => {
    const rows = [makeThread(1, { pinned_at: "2026-09-10T10:00:00.000000002Z" }), makeThread(2, { pinned_at: "2026-09-10T10:00:00.000000001Z" }), makeThread(3, { pinned_at: "2026-09-10T10:00:00.000000001Z" })];
    expect(threadTree(rows, "active", now, new Set()).map(row => row.thread.id)).toEqual([2,3,1]);
  });
  it("retains hidden parent context when filtering descendants without changing their lifecycle", () => {
    const forest = [makeThread(1, { archived_at: "2026-09-01T00:00:00Z" }), makeThread(2, { parent_thread_id: 1 }), makeThread(3, { parent_thread_id: 2, settled_at: "2026-09-01T00:00:00Z" })];
    expect(threadTree(forest, "settled", now, new Set()).map(row => [row.thread.id, row.context])).toEqual([[1,true],[2,true],[3,false]]);
    expect(forest[0].archived_at).not.toBeNull();
  });
  it("keeps missing-parent and cyclic snapshots reachable once with bounded ancestry", () => {
    const broken = [makeThread(1, { parent_thread_id: 90 }), makeThread(2, { parent_thread_id: 3 }), makeThread(3, { parent_thread_id: 2 })];
    expect(threadTree(broken, "active", now, new Set([1,2,3])).map(row => row.thread.id)).toEqual([1,2,3]);
    expect(threadAncestors(broken, 2).map(thread => thread.id)).toEqual([3]);
  });
});
