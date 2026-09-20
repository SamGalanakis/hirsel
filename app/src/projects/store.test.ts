import { flush } from "solid-js";
import { beforeEach, describe, expect, it } from "vitest";

import { makeThread } from "../threads/fixtures";
import {
  consumeTaskFocus,
  enterProject,
  projectState,
  resetProjects,
  stageTaskFocus,
  stepIntoWorker,
} from "./store";

const threads = [
  makeThread(1, { title: "Hirsel", kind: "space", parent_thread_id: null }),
  makeThread(2, {
    title: "Project chat contract",
    kind: "task",
    parent_thread_id: 1,
    description: "Implement the accepted contract.",
    instrument: { type: "text", text: "Ready" },
  }),
];

beforeEach(() => flush(resetProjects));

describe("project conversation state", () => {
  it("keeps project recipient, Task focus and worker pairing independent", () => {
    flush(() => enterProject(1));
    expect(projectState).toMatchObject({
      projectRecipientId: 1,
      taskFocus: null,
      workerPairingId: null,
    });

    flush(() => stepIntoWorker(threads, 2));
    expect(projectState).toMatchObject({
      projectRecipientId: 1,
      taskFocus: null,
      workerPairingId: 2,
    });

    flush(() => stageTaskFocus(threads, 2, "Accepted brief"));
    expect(projectState.projectRecipientId).toBe(1);
    expect(projectState.workerPairingId).toBeNull();
    expect(projectState.taskFocus).toEqual({
      task_thread_id: 2,
      snapshot: {
        title: "Project chat contract",
        brief: "Accepted brief",
        instrument_summary: JSON.stringify({ type: "text", text: "Ready" }),
      },
    });

    flush(consumeTaskFocus);
    expect(projectState.projectRecipientId).toBe(1);
    expect(projectState.taskFocus).toBeNull();
    expect(projectState.workerPairingId).toBeNull();
  });

  it("refuses to stage non-Tasks or Tasks without an owning project", () => {
    const rootTask = makeThread(3, { kind: "task", parent_thread_id: null });
    expect(stageTaskFocus([...threads, rootTask], 1, "not a Task")).toBeNull();
    expect(stageTaskFocus([...threads, rootTask], 3, "no project")).toBeNull();
    expect(projectState).toMatchObject({
      projectRecipientId: null,
      taskFocus: null,
      workerPairingId: null,
    });
  });
});
