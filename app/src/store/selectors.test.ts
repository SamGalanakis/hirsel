import { describe, expect, it } from "vitest";
import {
  partitionProcesses,
  runningProcessCount,
} from "./selectors";
import type { ProcessInfo } from "../protocol";

function proc(overrides: Partial<ProcessInfo> = {}): ProcessInfo {
  return {
    id: "proc-1",
    kind: "subagent",
    label: "task",
    agent: null,
    model: null,
    state: "running",
    started_ts: "2026-07-09T00:00:00Z",
    last_event_ts: "2026-07-09T00:00:00Z",
    summary: null,
    ...overrides,
  };
}

describe("runningProcessCount (Processes tab badge)", () => {
  it("counts only running processes, ignoring every terminal state", () => {
    expect(
      runningProcessCount([
        proc({ id: "a", state: "running" }),
        proc({ id: "b", state: "running" }),
        proc({ id: "c", state: "done" }),
        proc({ id: "d", state: "failed" }),
        proc({ id: "e", state: "cancelled" }),
        proc({ id: "f", state: "abandoned" }),
      ]),
    ).toBe(2);
  });

  it("is 0 for an empty list", () => {
    expect(runningProcessCount([])).toBe(0);
  });
});

describe("partitionProcesses (Running/Finished, newest activity first)", () => {
  it("splits on running vs terminal and sorts each by last_event_ts desc", () => {
    const { running, finished } = partitionProcesses([
      proc({ id: "r-old", state: "running", last_event_ts: "2026-07-09T00:00:01Z" }),
      proc({ id: "r-new", state: "running", last_event_ts: "2026-07-09T00:00:09Z" }),
      proc({ id: "f-old", state: "done", last_event_ts: "2026-07-09T00:00:02Z" }),
      proc({ id: "f-new", state: "failed", last_event_ts: "2026-07-09T00:00:08Z" }),
    ]);
    expect(running.map((p) => p.id)).toEqual(["r-new", "r-old"]);
    expect(finished.map((p) => p.id)).toEqual(["f-new", "f-old"]);
  });
});
