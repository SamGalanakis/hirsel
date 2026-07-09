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
