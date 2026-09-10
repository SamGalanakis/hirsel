import type { ChatMessage } from "../protocol";
import type { Thread, ThreadActivity, ThreadDetail, ThreadTurn } from "./types";

export interface ThreadHistory {
  brief: ThreadDetail["brief"];
  messages: ChatMessage[];
  turns: ThreadTurn[];
  activities: ThreadActivity[];
  hasMore: boolean;
  loaded: boolean;
}
export const emptyHistory = (): ThreadHistory => ({ brief: { text: "", artifact_ids: [] }, messages: [], turns: [], activities: [], hasMore: false, loaded: false });
export function upsertThread(threads: Thread[], incoming: Thread): Thread[] {
  const prior = threads.find(t => t.id === incoming.id);
  if (prior && prior.revision > incoming.revision) return threads;
  return [...threads.filter(t => t.id !== incoming.id), incoming].sort((a, b) => b.id - a.id);
}
export function mergeById<T extends { id: number }>(prior: T[], incoming: T[]): T[] {
  const entries = new Map(prior.map(row => [row.id, row]));
  for (const row of incoming) entries.set(row.id, row);
  return [...entries.values()].sort((a, b) => a.id - b.id);
}
/** A fetched page may race live updates: only replace older rows, never delete live arrivals. */
export function mergeDetail(prior: ThreadHistory, detail: ThreadDetail, earlier: boolean): ThreadHistory {
  const id = detail.thread.id;
  const turns = mergeById(detail.turns.filter(t => t.thread_id === id), prior.turns);
  const latestAssignment = (activities: ThreadActivity[]) => Math.max(-1, ...activities.filter(activity => activity.kind === "delegation_received").map(activity => activity.id));
  return {
    brief: latestAssignment(prior.activities) > latestAssignment(detail.activities) ? prior.brief : detail.brief,
    messages: mergeById(detail.messages.filter(m => m.thread_id === id), prior.messages),
    turns,
    activities: mergeById(detail.activities.filter(a => a.thread_id === id), prior.activities),
    hasMore: earlier || !prior.loaded ? detail.has_more : prior.hasMore,
    loaded: true,
  };
}
export type ThreadSection = "active" | "settled" | "snoozed" | "archived";
export function threadSection(thread: Thread, now = Date.now()): ThreadSection {
  if (thread.archived_at) return "archived";
  if (thread.settled_at) return "settled";
  if (thread.snoozed_until && Date.parse(thread.snoozed_until) > now) return "snoozed";
  return "active";
}
