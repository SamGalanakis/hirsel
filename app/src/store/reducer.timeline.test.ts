import { describe, expect, it } from "vitest";
import { reduce } from "./reducer";
import { initialState, type AppState } from "./types";
import type { ChatMessage, TurnEvent } from "../protocol";

/** Feed a turn_event through the reducer. */
function ev(state: AppState, seq: number, event: TurnEvent): AppState {
  return reduce(state, { type: "turn_event", payload: { type: "turn_event", seq, event } });
}

function agentMsg(id: number, toolCalls: ChatMessage["tool_calls"] = []): ChatMessage {
  return { id, author: "agent", body: "reply", ref: null, ts: `2026-07-09T00:00:0${id}Z`, tool_calls: toolCalls };
}

describe("turn_event accumulation + ordering", () => {
  it("keeps events sorted by seq regardless of arrival order", () => {
    let s = ev(initialState(), 3, { kind: "prose", text: "c" });
    s = ev(s, 1, { kind: "prose", text: "a" });
    s = ev(s, 2, { kind: "tool_start", id: "t1", name: "read_file", summary: "x.ts" });
    expect(s.turnEvents.map((e) => e.seq)).toEqual([1, 2, 3]);
  });

  it("treats a redelivered seq idempotently (replace, no duplicate)", () => {
    let s = ev(initialState(), 1, { kind: "prose", text: "a" });
    s = ev(s, 1, { kind: "prose", text: "a" });
    expect(s.turnEvents).toHaveLength(1);
  });

  it("tolerates gaps: a missing seq is simply absent, order preserved", () => {
    let s = ev(initialState(), 1, { kind: "prose", text: "a" });
    // seq 2 never arrives.
    s = ev(s, 3, { kind: "prose", text: "c" });
    expect(s.turnEvents.map((e) => e.seq)).toEqual([1, 3]);
  });
});

describe("turn_event clear boundaries", () => {
  it("does NOT clear on an owner message", () => {
    let s = ev(initialState(), 1, { kind: "prose", text: "a" });
    s = reduce(s, {
      type: "msg",
      payload: {
        type: "msg",
        message: { id: 5, author: "owner", body: "hi", ref: null, ts: "2026-07-09T00:00:05Z" },
      },
    });
    expect(s.turnEvents).toHaveLength(1);
  });

  it("clears on agent_activity idle but not on thinking", () => {
    let s = ev(initialState(), 1, { kind: "prose", text: "a" });
    s = reduce(s, { type: "agent_activity", payload: { state: "thinking", text: "…" } });
    expect(s.turnEvents).toHaveLength(1);
    s = reduce(s, { type: "agent_activity", payload: { state: "idle", text: null } });
    expect(s.turnEvents).toEqual([]);
  });
});

describe("turn details retention on commit", () => {
  it("freezes the live timeline onto the committing agent message, then clears it", () => {
    let s = ev(initialState(), 1, { kind: "prose", text: "looking… " });
    s = ev(s, 2, { kind: "tool_start", id: "t1", name: "read_file", summary: "x.ts" });
    s = ev(s, 3, { kind: "tool_done", id: "t1", name: "read_file", ok: true, summary: "read 10 lines" });
    s = ev(s, 4, { kind: "prose", text: "done." });

    s = reduce(s, { type: "msg", payload: { type: "msg", message: agentMsg(7) } });

    // Live buffer cleared; the finished timeline is retained under the msg id.
    expect(s.turnEvents).toEqual([]);
    expect(s.turnDetails[7].map((e) => e.seq)).toEqual([1, 2, 3, 4]);
    expect(s.turnDetails[7][1].event).toMatchObject({ kind: "tool_start", name: "read_file" });
  });

  it("does not create a turn-details entry when the turn had no events", () => {
    const s = reduce(initialState(), { type: "msg", payload: { type: "msg", message: agentMsg(7) } });
    expect(s.turnDetails).toEqual({});
  });

  it("retains details across a later resync (session memory, not sync state)", () => {
    let s = ev(initialState(), 1, { kind: "prose", text: "hi" });
    s = reduce(s, { type: "msg", payload: { type: "msg", message: agentMsg(7) } });
    expect(s.turnDetails[7]).toBeTruthy();
    s = reduce(s, {
      type: "hello_ok",
      payload: { type: "hello_ok", latest_msg_id: 7, messages: [], inbox: [] },
    });
    // Live events cleared by the resync, but the retained turn detail survives.
    expect(s.turnEvents).toEqual([]);
    expect(s.turnDetails[7]).toBeTruthy();
  });
});

describe("turn details: trailing-prose duplication trim", () => {
  function agentMsgBody(id: number, body: string): ChatMessage {
    return { id, author: "agent", body, ref: null, ts: `2026-07-10T00:00:0${id}Z`, tool_calls: [] };
  }

  it("drops the trailing prose block when it exactly matches the committed body", () => {
    let s = ev(initialState(), 1, { kind: "prose", text: "looking into it…" });
    s = ev(s, 2, { kind: "tool_start", id: "t1", name: "read_file", summary: "x.ts" });
    s = ev(s, 3, { kind: "prose", text: "Here is the answer." });

    s = reduce(s, {
      type: "msg",
      payload: { type: "msg", message: agentMsgBody(7, "Here is the answer.") },
    });

    expect(s.turnDetails[7].map((e) => e.seq)).toEqual([1, 2]);
  });

  it("trims when the streamed final prose is a prefix of the final body (lost trailing text)", () => {
    let s = ev(initialState(), 1, { kind: "tool_start", id: "t1", name: "read_file", summary: "x.ts" });
    s = ev(s, 2, { kind: "prose", text: "Here is the ans" });

    s = reduce(s, {
      type: "msg",
      payload: { type: "msg", message: agentMsgBody(7, "Here is the answer.") },
    });

    expect(s.turnDetails[7].map((e) => e.seq)).toEqual([1]);
  });

  it("trims when the committed body is a prefix of the streamed final prose (lost trailing whitespace)", () => {
    let s = ev(initialState(), 1, { kind: "tool_start", id: "t1", name: "read_file", summary: "x.ts" });
    s = ev(s, 2, { kind: "prose", text: "Here is the answer.   " });

    s = reduce(s, {
      type: "msg",
      payload: { type: "msg", message: agentMsgBody(7, "Here is the answer.") },
    });

    expect(s.turnDetails[7].map((e) => e.seq)).toEqual([1]);
  });

  it("keeps intermediate prose blocks untouched, only trimming the trailing one", () => {
    let s = ev(initialState(), 1, { kind: "prose", text: "First I'll check the file." });
    s = ev(s, 2, { kind: "tool_start", id: "t1", name: "read_file", summary: "x.ts" });
    s = ev(s, 3, { kind: "prose", text: "Final answer." });

    s = reduce(s, {
      type: "msg",
      payload: { type: "msg", message: agentMsgBody(7, "Final answer.") },
    });

    expect(s.turnDetails[7].map((e) => e.seq)).toEqual([1, 2]);
    expect(s.turnDetails[7][0].event).toMatchObject({ kind: "prose", text: "First I'll check the file." });
  });

  it("still attaches turn details when the trailing block is a tool event, not prose", () => {
    let s = ev(initialState(), 1, { kind: "prose", text: "Checking…" });
    s = ev(s, 2, { kind: "tool_start", id: "t1", name: "read_file", summary: "x.ts" });
    s = ev(s, 3, { kind: "tool_done", id: "t1", name: "read_file", ok: true, summary: "10 lines" });

    s = reduce(s, {
      type: "msg",
      payload: { type: "msg", message: agentMsgBody(7, "Done reading the file.") },
    });

    expect(s.turnDetails[7].map((e) => e.seq)).toEqual([1, 2, 3]);
  });

  it("does not attach turn details at all when trimming empties the whole timeline", () => {
    const s0 = ev(initialState(), 1, { kind: "prose", text: "Final answer." });
    const s = reduce(s0, {
      type: "msg",
      payload: { type: "msg", message: agentMsgBody(7, "Final answer.") },
    });

    expect(s.turnDetails).toEqual({});
    expect(s.turnDetails[7]).toBeUndefined();
  });
});

describe("turn details: clone-through hardening (mirrors sideChats' fix)", () => {
  function agentMsgBody(id: number, body: string): ChatMessage {
    return { id, author: "agent", body, ref: null, ts: `2026-07-10T00:00:0${id}Z`, tool_calls: [] };
  }

  it("retains a turn's events as a fresh array, not the same live turnEvents reference", () => {
    let s = ev(initialState(), 1, { kind: "tool_start", id: "t1", name: "read_file", summary: "x.ts" });
    const liveTurnEvents = s.turnEvents;
    s = reduce(s, { type: "msg", payload: { type: "msg", message: agentMsgBody(7, "done") } });

    expect(s.turnDetails[7]).toEqual(liveTurnEvents);
    expect(s.turnDetails[7]).not.toBe(liveTurnEvents);
  });

  it("clones every OTHER retained entry's array on a later commit rather than aliasing it", () => {
    let s = ev(initialState(), 1, { kind: "tool_start", id: "t1", name: "read_file", summary: "x.ts" });
    s = reduce(s, { type: "msg", payload: { type: "msg", message: agentMsgBody(7, "first") } });
    const firstEntryAfterFirstCommit = s.turnDetails[7];

    s = ev(s, 2, { kind: "tool_start", id: "t2", name: "grep", summary: "foo" });
    s = reduce(s, { type: "msg", payload: { type: "msg", message: agentMsgBody(8, "second") } });
    const firstEntryAfterSecondCommit = s.turnDetails[7];

    // Content-equal (message 7's frozen timeline never changes)...
    expect(firstEntryAfterSecondCommit).toEqual(firstEntryAfterFirstCommit);
    // ...but NOT the same reference: a fresh clone (rather than carrying over
    // the exact prior array) is what makes a subsequent store write safe —
    // see retainTurnDetails' note in reducer.ts.
    expect(firstEntryAfterSecondCommit).not.toBe(firstEntryAfterFirstCommit);
  });

  it("evicts the oldest retained turn once over the session cap, keeping the newest ones fresh", () => {
    let s = initialState();
    for (let i = 1; i <= 51; i++) {
      s = ev(s, i, { kind: "tool_start", id: `t${i}`, name: "read_file", summary: `${i}.ts` });
      s = reduce(s, { type: "msg", payload: { type: "msg", message: agentMsgBody(i, `reply ${i}`) } });
    }
    const ids = Object.keys(s.turnDetails).map(Number).sort((a, b) => a - b);
    expect(ids).toHaveLength(50);
    expect(ids[0]).toBe(2); // id 1 evicted, oldest of the 50 retained is 2.
    expect(s.turnDetails[51]).toBeTruthy();
  });
});
