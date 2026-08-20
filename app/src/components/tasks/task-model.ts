import type { EventItem } from "../../protocol";
import { isEventResolved } from "../../store/selectors";
import type { DisplayMessage } from "../../store/types";

export function taskName(task: EventItem): string {
  return task.name.replace(/^@/, "").replaceAll("-", " ");
}

/** What a Task is asking of the Owner right now — the one vocabulary the chip
 * label, the status dot, and the load-time focus rule all read from. */
export type TaskStatus = "done" | "blocked" | "needs-you" | "unseen" | "moving";

/** The single decision. Evaluated once per Task — callers hand in PROJECTED
 * events (`effectiveEvents()`), which already fold in optimistic overrides —
 * and everything downstream is a table lookup on the result. */
export function taskStatus(task: EventItem): TaskStatus {
  if (isEventResolved(task)) return "done";
  if (task.blocking) return "blocked";
  if (task.kind === "judgment") return "needs-you";
  if (task.read) return "moving";
  return "unseen";
}

/** Everything a status renders as, in one place: the word on the chip, the dot's
 * tone, and `rank` — how much the Task needs the Owner, lower = needs you more.
 * "done" ranks null: it is not a candidate for auto-focus at all, since landing
 * the Owner on work he already settled would be the opposite of most-needing.
 * Unread awareness ("unseen") sits between an open judgment and work merely in
 * motion: nothing is asked of the Owner, but he has not seen it yet.
 *
 * KNOWN DIVERGENCE (deliberate, do not "fix" here): "blocked" is gated on the
 * bare `task.blocking` flag, while `taskPriorityRank` in store/selectors.ts
 * gates its blocking band on `isOpenJudgment && blocking`. So a blocking
 * *summary* event shows danger-red here and wins auto-focus, yet sorts to the
 * awareness tail in the index. Both gates are preserved exactly as they were;
 * aligning them is a separate, behavior-changing decision. */
const TASK_STATUS: Record<TaskStatus, { label: string; tone: string; rank: number | null }> = {
  done: { label: "done", tone: "bg-status-success", rank: null },
  blocked: { label: "blocked on you", tone: "bg-status-danger", rank: 0 },
  "needs-you": { label: "needs you", tone: "bg-primary", rank: 1 },
  unseen: { label: "new", tone: "bg-status-attention", rank: 2 },
  moving: { label: "moving", tone: "bg-status-idle", rank: 3 },
};

/** The word shown on the chip (and read out in its accessible name). */
export function taskLabel(status: TaskStatus): string {
  return TASK_STATUS[status].label;
}

/** The status dot's background class. */
export function taskTone(status: TaskStatus): string {
  return TASK_STATUS[status].tone;
}

/** The one Task to open on load: the most-needing task, newest first within a
 * band (the freshest thing asking for you is the one you are landing on),
 * with the higher id as a stable final tiebreak. Pure and total — the shell
 * effect and its tests share this one rule. Returns null when nothing in the
 * field wants the Owner, which is the ambient field's cue to stay ambient. */
export function mostNeedingTask(tasks: EventItem[]): EventItem | null {
  let best: EventItem | null = null;
  let bestRank = Number.POSITIVE_INFINITY;
  let bestTs = Number.NEGATIVE_INFINITY;
  for (const task of tasks) {
    const rank = TASK_STATUS[taskStatus(task)].rank;
    if (rank === null) continue;
    const parsed = Date.parse(task.ts);
    // An unparseable ts must never outrank a real one; it sinks to the oldest.
    const ts = Number.isFinite(parsed) ? parsed : Number.NEGATIVE_INFINITY;
    if (best === null || rank < bestRank
      || (rank === bestRank && (ts > bestTs || (ts === bestTs && task.id > best.id)))) {
      best = task;
      bestRank = rank;
      bestTs = ts;
    }
  }
  return best;
}

/** Follow one Task's durable anchor/mention boundary through reply refs. A
 * message that starts or addresses another durable Task is a hard boundary,
 * even when its anchor happens to descend from this Task's anchor. */
export function messagesForTask(
  task: EventItem,
  messages: DisplayMessage[],
  tasks: EventItem[],
): DisplayMessage[] {
  const related = new Set<number>([task.anchor]);
  const blocked = new Set<number>();
  const otherTaskIds = new Set(tasks.filter((item) => item.id !== task.id).map((item) => item.id));
  const otherAnchors = new Set(tasks.filter((item) => item.id !== task.id).map((item) => item.anchor));
  const out: DisplayMessage[] = [];
  let awaitingAgent = false;
  for (const message of messages) {
    const mentions = message.mentions ?? [];
    const namesTarget = message.id === task.anchor || mentions.includes(task.id);
    const mentionsOther = mentions.some((id) => otherTaskIds.has(id));
    const crossesOtherTask = mentionsOther || (!namesTarget && (
      otherAnchors.has(message.id)
      || (message.ref !== null && otherAnchors.has(message.ref))
      || (message.ref !== null && blocked.has(message.ref))
    ));
    if (crossesOtherTask) {
      blocked.add(message.id);
      awaitingAgent = false;
      continue;
    }

    const directlyRelated = namesTarget || (message.ref !== null && related.has(message.ref));
    const implicitReply = message.author === "agent" && awaitingAgent;
    if (!directlyRelated && !implicitReply) {
      if (message.author === "owner") awaitingAgent = false;
      continue;
    }
    related.add(message.id);
    out.push(message);
    awaitingAgent = message.author === "owner";
  }
  return out;
}

export function taskSendContext(
  task: EventItem | null,
  ref: number | null,
  mentions: number[],
): { ref: number | null; mentions: number[] } {
  if (!task) return { ref, mentions };
  return {
    ref: task.anchor > 0 ? task.anchor : ref,
    mentions: Array.from(new Set([...mentions, task.id])),
  };
}
