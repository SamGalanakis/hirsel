import type { EventItem } from "../../protocol";
import { isEventResolved } from "../../store/selectors";
import type { DisplayMessage } from "../../store/types";

export function taskName(task: EventItem): string {
  return task.name.replace(/^@/, "").replaceAll("-", " ");
}

export function taskState(task: EventItem): string {
  if (isEventResolved(task)) return "done";
  if (task.blocking) return "blocked on you";
  if (task.kind === "judgment") return "needs you";
  if (task.read) return "moving";
  return "new";
}

export function taskTone(task: EventItem): string {
  if (isEventResolved(task)) return "bg-status-success";
  if (task.blocking) return "bg-status-danger";
  if (task.kind === "judgment") return "bg-primary";
  if (!task.read) return "bg-status-attention";
  return "bg-status-idle";
}

/** How much a Task needs the Owner, as a sortable rank over the SAME state
 * vocabulary `taskState` renders (lower = needs you more). "done" is not a
 * candidate at all and returns null: landing the Owner on work he already
 * settled would be the opposite of most-needing. */
function neediness(task: EventItem): number | null {
  switch (taskState(task)) {
    case "blocked on you": return 0;
    case "needs you": return 1;
    // Unread awareness sits between an open judgment and work merely in
    // motion: nothing is asked of the Owner, but he has not seen it yet.
    case "new": return 2;
    case "moving": return 3;
    default: return null; // "done"
  }
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
    const rank = neediness(task);
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
