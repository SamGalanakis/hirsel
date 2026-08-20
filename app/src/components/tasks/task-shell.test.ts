import { describe, expect, it } from "vitest";
import { EventKind } from "../../protocol";
import type { EventItem } from "../../protocol";
import type { DisplayMessage } from "../../store/types";
import {
  clearTaskFocus,
  focusTask,
  reconcileTaskFocus,
  state,
  toggleTaskFocus,
} from "../../store/store";
import { messagesForTask, mostNeedingTask, taskSendContext } from "./task-model";

const task = {
  id: 7,
  kind: EventKind.Judgment,
  source: { kind: "agent", ref: "host" },
  name: "@direction",
  description: "Choose a direction",
  requires_response: true,
  quick_replies: [],
  status: "open",
  read: false,
  anchor: 10,
  ts: "2026-07-23T10:00:00Z",
  ui: [],
} satisfies EventItem;

function message(
  id: number,
  author: "owner" | "agent",
  body: string,
  ref: number | null,
  mentions: number[] = [],
): DisplayMessage {
  return { id, author, body, ref, mentions, ts: `2026-07-23T10:0${id}:00Z` };
}

describe("messagesForTask", () => {
  it("follows the anchor and reply chain without admitting unrelated work", () => {
    const messages = [
      message(9, "owner", "Unrelated", null),
      message(10, "agent", "Task begins", null),
      message(11, "owner", "Explore this", 10),
      message(12, "agent", "Implicit immediate response", null),
      message(13, "agent", "Different global update", null),
      message(14, "owner", "Continue the task", 12),
    ];

    expect(messagesForTask(task, messages, [task]).map((item) => item.id)).toEqual([10, 11, 12, 14]);
  });

  it("stops at another durable task boundary without losing later deploy replies", () => {
    const deploy = { ...task, id: 1, name: "@deploy-4821", anchor: 2 };
    const auth = { ...task, id: 2, name: "@auth-pr", anchor: 3 };
    const messages = [
      message(1, "owner", "Anything need me?", null),
      message(2, "agent", "Deploy is staged. Ship it?", 1),
      // The mock's auth anchor descends from deploy's anchor. It is still a
      // hard durable boundary and must never appear in deploy.
      message(3, "agent", "Auth is ready. Open the PR?", 2),
      message(4, "owner", "Check the prod window", 2, [1]),
      message(5, "agent", "The window is clear.", null),
      message(6, "owner", "Assign the auth reviewer", 3, [2]),
      message(7, "agent", "Auth reviewer assigned.", null),
      message(8, "owner", "Global priority update", null),
      message(9, "agent", "Global priorities updated.", null),
      message(10, "owner", "One more deploy check", null, [1]),
      message(11, "agent", "Deploy check passed.", null),
      message(12, "agent", "Artifact signature is valid.", 11),
    ];

    expect(messagesForTask(deploy, messages, [deploy, auth]).map((item) => item.id))
      .toEqual([2, 4, 5, 10, 11, 12]);
    expect(messagesForTask(auth, messages, [deploy, auth]).map((item) => item.id))
      .toEqual([3, 6, 7]);
  });

  it("keeps a citing message in its own margin and shows it in the cited one", () => {
    const deploy = { ...task, id: 1, name: "@deploy-4821", anchor: 2 };
    const auth = { ...task, id: 2, name: "@auth-pr", anchor: 3 };
    const field = [deploy, auth];
    const messages = [
      message(2, "agent", "Deploy is staged. Ship it?", null),
      message(3, "agent", "Auth is ready. Open the PR?", null),
      // Written inside deploy, citing auth: `#2` typed into a deploy-focused
      // composer sends ref=deploy.anchor, mentions=[2, 1].
      message(4, "owner", "Hold until #2 lands", 2, [2, 1]),
      message(5, "agent", "Holding.", null),
    ];

    expect(messagesForTask(deploy, messages, field).map((item) => item.id))
      .toEqual([2, 4, 5]);
    // The reply follows its question into both margins: an agent line answering
    // a message that cited auth is auth's context too.
    expect(messagesForTask(auth, messages, field).map((item) => item.id))
      .toEqual([3, 4, 5]);
  });
});

describe("task focus", () => {
  it("maps task and global sends without leaking task identity", () => {
    expect(taskSendContext(task, 99, [3, task.id])).toEqual({ ref: task.anchor, mentions: [3, task.id] });
    expect(taskSendContext(null, 99, [3])).toEqual({ ref: 99, mentions: [3] });
  });

  it("starts ambient and toggles one task into and out of focus", () => {
    clearTaskFocus();
    expect(state.focusedTaskId).toBeNull();
    toggleTaskFocus(task.id);
    expect(state.focusedTaskId).toBe(task.id);
    toggleTaskFocus(task.id);
    expect(state.focusedTaskId).toBeNull();
  });

  it("moves focus straight to another task without passing through ambient", () => {
    clearTaskFocus();
    toggleTaskFocus(task.id);
    toggleTaskFocus(8);
    expect(state.focusedTaskId).toBe(8);
    clearTaskFocus();
  });

  it("returns to ambient when the focused task disappears", () => {
    toggleTaskFocus(task.id);
    reconcileTaskFocus([8]);
    expect(state.focusedTaskId).toBeNull();
  });

  it("keeps focus while the focused task is still in the field", () => {
    toggleTaskFocus(task.id);
    reconcileTaskFocus([8, task.id]);
    expect(state.focusedTaskId).toBe(task.id);
    clearTaskFocus();
  });

  it("focuses a task outright, without the toggle's off-state", () => {
    clearTaskFocus();
    focusTask(task.id);
    focusTask(task.id);
    expect(state.focusedTaskId).toBe(task.id);
    clearTaskFocus();
  });
});

describe("mostNeedingTask", () => {
  // The load-time choice, over the SAME vocabulary taskStatus renders.
  const candidate = (id: number, over: Partial<EventItem> = {}): EventItem => ({
    ...task,
    id,
    anchor: id,
    ts: `2026-07-23T1${id}:00:00Z`,
    ...over,
  } as EventItem);

  const blocked = (id: number, over: Partial<EventItem> = {}) =>
    candidate(id, { blocking: true, ...over });
  const needsYou = (id: number, over: Partial<EventItem> = {}) => candidate(id, over);
  const unseen = (id: number, over: Partial<EventItem> = {}) =>
    candidate(id, { kind: EventKind.Summary, requires_response: false, read: false, ...over });
  const movingTask = (id: number, over: Partial<EventItem> = {}) =>
    candidate(id, { kind: EventKind.Summary, requires_response: false, read: true, ...over });

  it("has no candidate in an empty field", () => {
    expect(mostNeedingTask([])).toBeNull();
  });

  it("ranks blocked on you over needs you over unseen over moving", () => {
    const field = [movingTask(1), unseen(2), needsYou(3), blocked(4)];
    expect(mostNeedingTask(field)?.id).toBe(4);
    expect(mostNeedingTask([movingTask(1), unseen(2), needsYou(3)])?.id).toBe(3);
    expect(mostNeedingTask([movingTask(1), unseen(2)])?.id).toBe(2);
    expect(mostNeedingTask([movingTask(1)])?.id).toBe(1);
  });

  it("breaks a tie within a band on the newest ts, then the higher id", () => {
    expect(mostNeedingTask([blocked(1), blocked(2), blocked(3)])?.id).toBe(3);
    const sameTs = [
      blocked(4, { ts: "2026-07-23T10:00:00Z" }),
      blocked(5, { ts: "2026-07-23T10:00:00Z" }),
    ];
    expect(mostNeedingTask(sameTs)?.id).toBe(5);
    // An unparseable ts must never outrank a real one.
    expect(mostNeedingTask([blocked(6, { ts: "not-a-date" }), blocked(7)])?.id).toBe(7);
  });

  it("never lands the Owner on settled work", () => {
    // Settled work reaches here already projected to done (wire or optimistic).
    const settled = blocked(1, { status: "done" });
    expect(mostNeedingTask([settled])).toBeNull();
    expect(mostNeedingTask([settled, movingTask(2)])?.id).toBe(2);
  });
});
