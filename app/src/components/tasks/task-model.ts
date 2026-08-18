import type { EventItem } from "../../protocol";
import { isEventResolved } from "../../store/selectors";
import type { DisplayMessage } from "../../store/types";

export function taskName(task: EventItem): string {
  return task.name.replace(/^@/, "").replaceAll("-", " ");
}

export function taskState(task: EventItem, decideOverrides: number[] = []): string {
  if (isEventResolved(task, decideOverrides)) return "done";
  if (task.blocking) return "blocked on you";
  if (task.kind === "judgment") return "needs you";
  if (task.read) return "moving";
  return "new";
}

export function taskTone(task: EventItem, decideOverrides: number[] = []): string {
  if (isEventResolved(task, decideOverrides)) return "bg-status-success";
  if (task.blocking) return "bg-status-danger";
  if (task.kind === "judgment") return "bg-primary";
  if (!task.read) return "bg-status-attention";
  return "bg-status-idle";
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
