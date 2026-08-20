import { describe, expect, it } from "vitest";
import { reduce } from "./reducer";
import { type AppState, initialState } from "./types";
import type { ChatMessage, TurnEvent } from "../protocol";
import { splitStreamingReply } from "../components/chat/timeline";

function msg(id: number, body: string, author: "owner" | "agent" = "agent"): ChatMessage {
  return { id, author, body, ref: null, ts: "2026-08-18T00:00:00Z" };
}

function hello(state: AppState, messages: ChatMessage[], latest = messages.length): AppState {
  return reduce(state, {
    type: "hello_ok",
    payload: { type: "hello_ok", latest_msg_id: latest, messages, pings: [] },
  });
}

function turn(state: AppState, seq: number, event: TurnEvent): AppState {
  return reduce(state, { type: "turn_event", payload: { type: "turn_event", seq, event } });
}

function commit(state: AppState, message: ChatMessage): AppState {
  return reduce(state, { type: "msg", payload: { type: "msg", message } });
}

describe("history survives a reload", () => {
  it("hydrates the conversation from a hello_ok replay window", () => {
    // The reload case: a fresh page has no messages and its stored cursor says
    // it had seen everything. The host now replays the recent window anyway, so
    // the conversation must come back rather than render empty.
    const window = [msg(1, "one", "owner"), msg(2, "two"), msg(3, "three", "owner")];
    const state = hello(initialState(), window, 3);

    expect(state.messages.map((m) => m.body)).toEqual(["one", "two", "three"]);
    expect(state.lastSeenMsgId).toBe(3);
  });

  it("re-replaying rows the client already holds is idempotent, not duplicated", () => {
    const window = [msg(1, "one"), msg(2, "two")];
    let state = hello(initialState(), window, 2);
    state = commit(state, msg(3, "three"));
    // A reconnect replays the same window again, now including id 3.
    state = hello(state, [msg(1, "one"), msg(2, "two"), msg(3, "three")], 3);

    expect(state.messages.map((m) => m.id)).toEqual([1, 2, 3]);
  });

  it("keeps local history older than the replay window", () => {
    let state = hello(initialState(), [msg(1, "old"), msg(2, "older")], 2);
    // A later handshake whose window starts at 2 must not wipe id 1.
    state = hello(state, [msg(2, "older"), msg(3, "new")], 3);

    expect(state.messages.map((m) => m.id)).toEqual([1, 2, 3]);
  });
});

describe("streamed reply commits exactly once", () => {
  it("accumulates prose deltas into one in-flight reply", () => {
    let state = initialState();
    state = turn(state, 1, { kind: "prose", text: "Hel" });
    state = turn(state, 2, { kind: "prose", text: "lo th" });
    state = turn(state, 3, { kind: "prose", text: "ere" });

    expect(splitStreamingReply(state.turnEvents).reply).toBe("Hello there");
    expect(splitStreamingReply(state.turnEvents).activity).toHaveLength(0);
  });

  it("keeps tool work in the activity timeline and only the trailing run as the reply", () => {
    let state = initialState();
    state = turn(state, 1, { kind: "prose", text: "Looking now." });
    state = turn(state, 2, { kind: "tool_start", id: "t1", name: "read_file", summary: null });
    state = turn(state, 3, { kind: "tool_done", id: "t1", name: "read_file", ok: true, summary: null });
    state = turn(state, 4, { kind: "prose", text: "Found it." });

    const split = splitStreamingReply(state.turnEvents);
    expect(split.reply).toBe("Found it.");
    expect(split.activity).toHaveLength(3);
  });

  it("shows no in-flight reply while the turn is mid tool call", () => {
    let state = initialState();
    state = turn(state, 1, { kind: "prose", text: "Checking." });
    state = turn(state, 2, { kind: "tool_start", id: "t1", name: "grep", summary: null });

    expect(splitStreamingReply(state.turnEvents).reply).toBe("");
  });

  it("replaces the draft with the committed row — no duplicate, no second copy", () => {
    let state = initialState();
    state = turn(state, 1, { kind: "prose", text: "Hello " });
    state = turn(state, 2, { kind: "prose", text: "there" });
    expect(splitStreamingReply(state.turnEvents).reply).toBe("Hello there");

    state = commit(state, msg(1, "Hello there"));

    // The draft is gone (turnEvents cleared) and the body exists exactly once.
    expect(state.turnEvents).toHaveLength(0);
    expect(splitStreamingReply(state.turnEvents).reply).toBe("");
    expect(state.messages.filter((m) => m.body === "Hello there")).toHaveLength(1);
  });

  it("does not park a pure-prose turn into turn details (it would restate the reply)", () => {
    let state = initialState();
    state = turn(state, 1, { kind: "prose", text: "Hello there" });
    state = commit(state, msg(1, "Hello there"));

    expect(state.turnDetails[1]).toBeUndefined();
  });
});

describe("reconnect mid-turn leaves no orphaned draft", () => {
  it("drops a half-streamed reply on the handshake", () => {
    let state = initialState();
    state = turn(state, 1, { kind: "prose", text: "half writ" });
    expect(splitStreamingReply(state.turnEvents).reply).toBe("half writ");

    // The socket drops and re-handshakes. Turn deltas are ephemeral and never
    // replayed, so a partial draft must not survive as a phantom reply.
    state = hello(state, [msg(1, "one")], 1);

    expect(state.turnEvents).toHaveLength(0);
    expect(state.lastTurnEvents).toHaveLength(0);
    expect(splitStreamingReply(state.turnEvents).reply).toBe("");
  });

  it("clears the draft when the turn goes idle without committing", () => {
    let state = initialState();
    state = turn(state, 1, { kind: "prose", text: "abandoned" });
    state = reduce(state, {
      type: "agent_activity",
      payload: { state: "idle", text: null },
    });

    expect(splitStreamingReply(state.turnEvents).reply).toBe("");
  });
});
