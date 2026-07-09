import { describe, expect, it } from "vitest";
import { reduce } from "./reducer";
import { initialState } from "./types";
import type { ChatMessage, ProcessInfo } from "../protocol";

function proc(overrides: Partial<ProcessInfo> = {}): ProcessInfo {
  return {
    id: "proc-1",
    kind: "subagent",
    label: "Do the thing",
    agent: "code-reviewer",
    model: "gpt-5.5",
    state: "running",
    started_ts: "2026-07-09T00:00:00Z",
    last_event_ts: "2026-07-09T00:00:00Z",
    summary: null,
    ...overrides,
  };
}

function agentMsg(id: number, toolCalls: ChatMessage["tool_calls"] = []): ChatMessage {
  return {
    id,
    author: "agent",
    body: "reply",
    ref: null,
    ts: `2026-07-09T00:00:0${id}Z`,
    tool_calls: toolCalls,
  };
}

describe("hello_ok seeds processes", () => {
  it("seeds processes from the payload (and defaults to [])", () => {
    const withProcs = reduce(initialState(), {
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: 0,
        messages: [],
        inbox: [],
        processes: [proc(), proc({ id: "proc-2", kind: "monitor" })],
      },
    });
    expect(withProcs.processes.map((p) => p.id)).toEqual(["proc-1", "proc-2"]);

    const withoutProcs = reduce(initialState(), {
      type: "hello_ok",
      payload: { type: "hello_ok", latest_msg_id: 0, messages: [], inbox: [] },
    });
    expect(withoutProcs.processes).toEqual([]);
  });

  it("clears any live tool calls at the resync boundary", () => {
    const seeded = reduce(initialState(), {
      type: "agent_tool_call",
      payload: { type: "agent_tool_call", name: "read_file", summary: null, seq: 1 },
    });
    expect(seeded.liveToolCalls).toHaveLength(1);
    const resynced = reduce(seeded, {
      type: "hello_ok",
      payload: { type: "hello_ok", latest_msg_id: 0, messages: [], inbox: [] },
    });
    expect(resynced.liveToolCalls).toEqual([]);
  });
});

describe("process_upsert", () => {
  it("appends a new process and updates an existing one in place", () => {
    const s1 = reduce(initialState(), {
      type: "process_upsert",
      payload: { type: "process_upsert", process: proc({ summary: "starting…" }) },
    });
    expect(s1.processes).toHaveLength(1);
    expect(s1.processes[0].summary).toBe("starting…");

    const s2 = reduce(s1, {
      type: "process_upsert",
      payload: {
        type: "process_upsert",
        process: proc({ state: "done", summary: "finished" }),
      },
    });
    // Same id → replaced in place (no duplicate row), new state/summary applied.
    expect(s2.processes).toHaveLength(1);
    expect(s2.processes[0].state).toBe("done");
    expect(s2.processes[0].summary).toBe("finished");

    const s3 = reduce(s2, {
      type: "process_upsert",
      payload: { type: "process_upsert", process: proc({ id: "proc-2", kind: "monitor" }) },
    });
    expect(s3.processes.map((p) => p.id)).toEqual(["proc-1", "proc-2"]);
  });
});

describe("agent_tool_call live-then-clear", () => {
  const call = (seq: number, name: string) =>
    ({ type: "agent_tool_call", name, summary: null, seq }) as const;

  it("accumulates by seq, deduping and keeping sorted order", () => {
    let s = reduce(initialState(), { type: "agent_tool_call", payload: call(2, "grep") });
    s = reduce(s, { type: "agent_tool_call", payload: call(1, "read_file") });
    expect(s.liveToolCalls.map((c) => c.seq)).toEqual([1, 2]);
    expect(s.liveToolCalls.map((c) => c.name)).toEqual(["read_file", "grep"]);

    // Redelivery of the same seq replaces rather than duplicates.
    s = reduce(s, { type: "agent_tool_call", payload: call(1, "read_file") });
    expect(s.liveToolCalls).toHaveLength(2);
  });

  it("clears live tool calls when an agent message commits the turn", () => {
    let s = reduce(initialState(), { type: "agent_tool_call", payload: call(1, "read_file") });
    s = reduce(s, { type: "msg", payload: { type: "msg", message: agentMsg(1) } });
    expect(s.liveToolCalls).toEqual([]);
  });

  it("does NOT clear live tool calls on an owner message", () => {
    let s = reduce(initialState(), { type: "agent_tool_call", payload: call(1, "read_file") });
    s = reduce(s, {
      type: "msg",
      payload: {
        type: "msg",
        message: { id: 5, author: "owner", body: "hi", ref: null, ts: "2026-07-09T00:00:05Z" },
      },
    });
    expect(s.liveToolCalls).toHaveLength(1);
  });

  it("clears on agent_activity idle but not on thinking", () => {
    let s = reduce(initialState(), { type: "agent_tool_call", payload: call(1, "read_file") });
    s = reduce(s, {
      type: "agent_activity",
      payload: { state: "thinking", text: "…" },
    });
    expect(s.liveToolCalls).toHaveLength(1);
    s = reduce(s, { type: "agent_activity", payload: { state: "idle", text: null } });
    expect(s.liveToolCalls).toEqual([]);
  });
});
