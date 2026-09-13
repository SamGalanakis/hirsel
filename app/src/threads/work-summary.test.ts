import { describe, expect, it } from "vitest";
import { quietWakeTurn } from "./work-summary";
import type { TimelineEvent } from "../store/types";
import type { ThreadTurn } from "./types";
const turn = (state: ThreadTurn["state"]): ThreadTurn => ({ id: 9, thread_id: 1, requester_thread_id: null, requester_turn_id: null, owner_message_id: null, agent_message_id: null, state, started_at: "2026-09-09T10:00:00Z", finished_at: state === "completed" ? "2026-09-09T10:00:07Z" : null });
/** The wake a child report triggers: one trivial program, nothing said. */
const wake: TimelineEvent[] = [
  { seq: 1, event: { kind: "code_start", id: "cell-1", language: "typescript", code: 'finish("")', truncated: false } },
  { seq: 2, event: { kind: "code_done", id: "cell-1", ok: true, summary: null } },
];
describe("quiet wake turns", () => {
  it("hides a completed turn whose only events are a trivial program", () => {
    expect(quietWakeTurn(turn("completed"), [], wake, false)).toBe(true);
    expect(quietWakeTurn(turn("completed"), [], [], false)).toBe(true);
  });
  it("keeps every turn that still has something to show", () => {
    expect(quietWakeTurn(turn("running"), [], wake, false)).toBe(false);
    expect(quietWakeTurn(turn("failed"), [], wake, false)).toBe(false);
    expect(quietWakeTurn(turn("queued"), [], wake, false)).toBe(false);
    expect(quietWakeTurn(undefined, [], wake, false)).toBe(false);
    // Show agent code asks for exactly these cells, so they earn their card back.
    expect(quietWakeTurn(turn("completed"), [], wake, true)).toBe(false);
    expect(quietWakeTurn(turn("completed"), [{ artifact_ids: [], id: 1, thread_id: 1, turn_id: 9, kind: "info", data: {}, ts: "2026-09-09T10:00:01Z" }], wake, false)).toBe(false);
    expect(quietWakeTurn(turn("completed"), [], [...wake, { seq: 3, event: { kind: "prose", text: "Done." } }], false)).toBe(false);
    expect(quietWakeTurn(turn("completed"), [], [{ seq: 1, event: { kind: "tool_start", id: "t1", name: "read_file", summary: null, input: null } }], false)).toBe(false);
  });
});
