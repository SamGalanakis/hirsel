import type { ChatMessage } from "../protocol";
import type { ThreadHistory } from "./model";
import type { ThreadActivity, ThreadTurn } from "./types";
export type ConversationEntry =
  | { key: string; kind: "message"; message: ChatMessage; turn?: ThreadTurn }
  | { key: string; kind: "turn"; turn: ThreadTurn }
  | { key: string; kind: "activity"; activity: ThreadActivity };
function record(value: unknown): Record<string, unknown> { return value !== null && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {}; }
/** Plugin envelopes are a current protocol shape, selected by kind. */
export function activityText(activity: ThreadActivity): string {
  const data = activity.kind.startsWith("plugin.") ? record(record(activity.data).payload) : record(activity.data);
  const kind = activity.kind.replace(/^plugin\./, "");
  const fields = kind === "child_report" ? [data.summary] : kind === "delegation_received" ? [data.brief] : kind === "info" || kind === "summary" ? [data.description, data.content_md] : kind === "process_completed" ? [data.summary] : kind === "scheduled_digest" ? [data.text] : [];
  return fields.filter((value): value is string => typeof value === "string" && value.length > 0).join("\n\n");
}
export function ownerFacingActivity(activity: ThreadActivity): boolean { return activity.kind === "child_report" || activity.kind === "delegation_received" || activity.artifact_ids.length > 0 || activityText(activity).length > 0; }
/** Ownership joins only exact IDs. Timestamps position independent background
 * entries; they never associate a message or activity with a guessed turn. */
export function instant(timestamp: string): bigint {
  const match = /^(.*T\d{2}:\d{2}:\d{2})(?:\.(\d+))?(Z|[+-]\d{2}:\d{2})$/.exec(timestamp);
  if (!match) throw new Error("Invalid activity timestamp");
  return BigInt(Date.parse(match[1] + match[3])) * 1_000_000n + BigInt((match[2] ?? "").padEnd(9, "0").slice(0, 9));
}
export function conversationEntries(history: ThreadHistory): ConversationEntry[] {
  const loaded = new Set(history.messages.map(message => message.id));
  const oldest = history.hasMore && history.messages.length ? instant(history.messages[0].ts) : null;
  const inPage = (timestamp: string) => oldest === null || instant(timestamp) >= oldest;
  const finals = new Map(history.turns.filter(turn => turn.agent_message_id !== null).map(turn => [turn.agent_message_id!, turn]));
  const unfinished = history.turns.filter(turn => turn.agent_message_id === null && (turn.owner_message_id !== null ? loaded.has(turn.owner_message_id) : inPage(turn.started_at)));
  const owned = new Map<number, ThreadTurn[]>();
  for (const turn of unfinished) if (turn.owner_message_id !== null) owned.set(turn.owner_message_id, [...owned.get(turn.owner_message_id) ?? [], turn]);
  const positioned: { entry: ConversationEntry; time: bigint; order: number }[] = [];
  for (const message of history.messages) {
    const time = instant(message.ts);
    const turn = finals.get(message.id);
    positioned.push({ entry: { key: turn ? `turn-${turn.id}` : `message-${message.id}`, kind: "message", message, turn }, time, order: 0 });
    for (const turn of owned.get(message.id) ?? []) positioned.push({ entry: { key: `turn-${turn.id}`, kind: "turn", turn }, time, order: 1 });
  }
  for (const turn of unfinished) if (turn.owner_message_id === null) positioned.push({ entry: { key: `turn-${turn.id}`, kind: "turn", turn }, time: instant(turn.started_at), order: 1 });
  const visibleTurns = new Set([...finals.values()].filter(turn => loaded.has(turn.agent_message_id!)).map(turn => turn.id).concat(unfinished.map(turn => turn.id)));
  for (const activity of history.activities) {
    if (!inPage(activity.ts) || (activity.turn_id !== null && !visibleTurns.has(activity.turn_id))) continue;
    if (ownerFacingActivity(activity) || activity.turn_id === null) positioned.push({ entry: { key: `activity-${activity.id}`, kind: "activity", activity }, time: instant(activity.ts), order: 2 });
  }
  return positioned.sort((a,b) => (a.time < b.time ? -1 : a.time > b.time ? 1 : a.order - b.order || Number(a.entry.key.split("-").at(-1)) - Number(b.entry.key.split("-").at(-1)))).map(row => row.entry);
}
