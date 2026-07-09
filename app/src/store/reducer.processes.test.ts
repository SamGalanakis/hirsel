import { describe, expect, it } from "vitest";
import { reduce } from "./reducer";
import { initialState } from "./types";
import type { ProcessInfo } from "../protocol";

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

  it("clears any live turn events at the resync boundary", () => {
    const seeded = reduce(initialState(), {
      type: "turn_event",
      payload: { type: "turn_event", seq: 1, event: { kind: "prose", text: "hi" } },
    });
    expect(seeded.turnEvents).toHaveLength(1);
    const resynced = reduce(seeded, {
      type: "hello_ok",
      payload: { type: "hello_ok", latest_msg_id: 0, messages: [], inbox: [] },
    });
    expect(resynced.turnEvents).toEqual([]);
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

