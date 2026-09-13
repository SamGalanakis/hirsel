import { describe, expect, it } from "vitest";
import { reduce } from "./reducer";
import { initialState } from "./types";
import type { ProcessInfo } from "../protocol";

function proc(overrides: Partial<ProcessInfo> = {}): ProcessInfo {
  return { thread_id: 1,
    id: "proc-1",
    name: "Do the thing",
    trigger: null,
    trigger_subscription_key: null,
    trigger_revision: null,
    trigger_enabled: null,
    cancellable: true,
    state: "running",
    started_ts: "2026-07-09T00:00:00Z",
    last_event_ts: "2026-07-09T00:00:00Z",
    last_fired_ts: null,
    last_outcome: null,
    ...overrides,
  };
}

describe("hello_ok seeds processes", () => {
  it("seeds processes from the payload (and defaults to [])", () => {
    const withProcs = reduce(initialState(), { type: "hello_ok", payload: { type: "hello_ok", processes: [proc(), proc({ id: "proc-2" })], history_id: "test-history", threads: [], views: [], host_version: "test", model: null, subagent_models: null, prompts: null, providers: null } });
    expect(withProcs.processes.map((p) => p.id)).toEqual(["proc-1", "proc-2"]);

    const withoutProcs = reduce(initialState(), { type: "hello_ok", payload: { type: "hello_ok", history_id: "test-history", threads: [], processes: [], views: [], host_version: "test", model: null, subagent_models: null, prompts: null, providers: null } });
    expect(withoutProcs.processes).toEqual([]);
  });
});

describe("process_upsert", () => {
  it("appends a new process and updates an existing one in place", () => {
    const s1 = reduce(initialState(), {
      type: "process_upsert",
      payload: { type: "process_upsert", process: proc({ last_outcome: "starting…" }) },
    });
    expect(s1.processes).toHaveLength(1);
    expect(s1.processes[0].last_outcome).toBe("starting…");

    const s2 = reduce(s1, {
      type: "process_upsert",
      payload: {
        type: "process_upsert",
        process: proc({ state: "done", last_outcome: "finished" }),
      },
    });
    // Same id → replaced in place (no duplicate row), new state/outcome applied.
    expect(s2.processes).toHaveLength(1);
    expect(s2.processes[0].state).toBe("done");
    expect(s2.processes[0].last_outcome).toBe("finished");

    const s3 = reduce(s2, {
      type: "process_upsert",
      payload: { type: "process_upsert", process: proc({ id: "proc-2" }) },
    });
    expect(s3.processes.map((p) => p.id)).toEqual(["proc-1", "proc-2"]);
  });
});
