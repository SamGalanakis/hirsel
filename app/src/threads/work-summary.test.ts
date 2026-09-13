import { describe, expect, it } from "vitest";
import { quietWakeTurn, workDuration } from "./work-summary";
import type { TimelineEvent } from "../store/types";
import type { ThreadTurn } from "./types";
const turn = (state: ThreadTurn["state"]): ThreadTurn => ({ id: 9, thread_id: 1, requester_thread_id: null, requester_turn_id: null, owner_message_id: null, agent_message_id: null, state, accepted_at: "2026-09-09T10:00:00Z", started_at: state === "queued" ? null : "2026-09-09T10:00:00Z", finished_at: state === "completed" ? "2026-09-09T10:00:07Z" : null });
/** The wake a child report triggers: one trivial program, nothing said. */
const wake: TimelineEvent[] = [
  { seq: 1, event: { kind: "code_start", id: "cell-1", language: "typescript", code: 'finish("")', truncated: false } },
  { seq: 2, event: { kind: "code_done", id: "cell-1", ok: true, summary: null } },
];
describe("quiet wake turns", () => {
  it("hides a completed turn whose only events are a trivial program", () => {
    expect(quietWakeTurn(turn("completed"), [], wake)).toBe(true);
    expect(quietWakeTurn(turn("completed"), [], [])).toBe(true);
  });
  it("keeps every turn that still has something to show", () => {
    expect(quietWakeTurn(turn("running"), [], wake)).toBe(false);
    expect(quietWakeTurn(turn("failed"), [], wake)).toBe(false);
    expect(quietWakeTurn(turn("queued"), [], wake)).toBe(false);
    expect(quietWakeTurn(undefined, [], wake)).toBe(false);
    // A program with anything in it beyond the bare finish is a Code entry, and
    // a turn with a Code entry has something to show.
    expect(quietWakeTurn(turn("completed"), [], [
      { seq: 1, event: { kind: "code_start", id: "cell-1", language: "typescript", code: 'finish(await shell.run({ cmd: "true" }))', truncated: false } },
      { seq: 2, event: { kind: "code_done", id: "cell-1", ok: true, summary: null } },
    ])).toBe(false);
    expect(quietWakeTurn(turn("completed"), [{ artifact_ids: [], id: 1, thread_id: 1, turn_id: 9, kind: "info", data: {}, ts: "2026-09-09T10:00:01Z" }], wake)).toBe(false);
    expect(quietWakeTurn(turn("completed"), [], [...wake, { seq: 3, event: { kind: "prose", text: "Done." } }])).toBe(false);
    expect(quietWakeTurn(turn("completed"), [], [{ seq: 1, event: { kind: "tool_start", id: "t1", name: "read_file", summary: null, input: null } }])).toBe(false);
  });
});

it("never counts queue time as execution duration", () => {
  const accepted_at = "2026-09-09T09:00:00Z";
  expect(workDuration({ ...turn("cancelled"), accepted_at, started_at: null, finished_at: "2026-09-09T10:00:00Z" }, Date.now())).toBe("");
  expect(workDuration({ ...turn("completed"), accepted_at }, Date.now())).toBe("7s");
});
