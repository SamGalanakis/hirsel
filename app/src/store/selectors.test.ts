import { describe, expect, it } from "vitest";
import {
  partitionProcesses,
  runningProcessCount,
  scopedProcesses,
} from "./selectors";
import type { ProcessInfo } from "../protocol";
import type { Thread } from "../threads/types";

function proc(overrides: Partial<ProcessInfo> = {}): ProcessInfo {
  return { thread_id: 1,
    id: "proc-1",
    name: "task",
    trigger: "every 30s",
    trigger_subscription_key: "timer-task",
    trigger_revision: 1,
    trigger_enabled: true,
    cancellable: true,
    state: "running",
    started_ts: "2026-07-09T00:00:00Z",
    last_event_ts: "2026-07-09T00:00:00Z",
    last_fired_ts: null,
    last_outcome: null,
    ...overrides,
  };
}

describe("runningProcessCount (Processes tab badge)", () => {
  it("counts only running processes, ignoring every terminal state", () => {
    expect(
      runningProcessCount([
        proc({ id: "a", state: "running" }),
        proc({ id: "b", state: "running" }),
        proc({ id: "w", state: "waiting" }),
        proc({ id: "c", state: "done" }),
        proc({ id: "d", state: "failed" }),
        proc({ id: "e", state: "cancelled" }),
        proc({ id: "f", state: "abandoned" }),
      ]),
    ).toBe(3);
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

describe("scopedProcesses", () => {
  it("includes the focused Thread and descendants but excludes peers and ancestors", () => {
    const thread = (id: number, parent_thread_id: number | null) => ({ id, parent_thread_id }) as Thread;
    const result = scopedProcesses(
      [proc({ id: "ancestor", thread_id: 1 }), proc({ id: "focused", thread_id: 2 }), proc({ id: "child", thread_id: 3 }), proc({ id: "peer", thread_id: 4 })],
      [thread(1, null), thread(2, 1), thread(3, 2), thread(4, 1)],
      2,
    );
    expect(result.map(process => process.id)).toEqual(["focused", "child"]);
  });
});
