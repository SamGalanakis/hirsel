import { stripInlineMarkdown } from "../components/Markdown";
import type { ThreadHistory } from "./model";
import { elapsedTime, threadStatus } from "./status";
import type { Thread } from "./types";

/** What the overview queue and the tree's "Needs you" band both read. One
 * selector, so the band, the header pill and the queue can never disagree
 * about which Threads are waiting on the Owner. */
export type AttentionGroup = "attention" | "running" | "recent";
export interface AttentionEntry {
  thread: Thread;
  group: AttentionGroup;
  /** How long this Thread has been in its current state, e.g. "4m". */
  waited: string | null;
  /** The Agent's last words, reduced to one readable line. */
  excerpt: string;
}

/** A Thread is waiting on the Owner when it says so, is not archived, and is
 * not sleeping through a snooze. */
export function needsOwner(thread: Thread, now: number): boolean {
  return !thread.archived_at && thread.attention === "needs_owner"
    && !(thread.snoozed_until && Date.parse(thread.snoozed_until) > now);
}

/** The question, not the monologue: the last sentence of the Agent's last
 * message that actually asks something, else its opening line. */
export function attentionExcerpt(body: string | undefined, limit = 160): string {
  // Line by line: the shared stripper concatenates blocks, and a queue row that
  // reads "…the logs.Should I roll back?" is worse than no excerpt at all.
  const text = (body ?? "").split(/\n+/u).map(line => stripInlineMarkdown(line)).filter(Boolean).join(" ").replace(/\s+/gu, " ").trim();
  if (!text) return "";
  // Split on sentence ends followed by space, so a version number ("1.4?")
  // stays inside its own question.
  const sentences = text.split(/(?<=[.!?])\s+/u);
  const chosen = (sentences.findLast(sentence => sentence.trimEnd().endsWith("?")) ?? text).trim();
  return chosen.length > limit ? `${chosen.slice(0, limit - 1).trimEnd()}…` : chosen;
}

function lastAgentBody(history: ThreadHistory | undefined): string | undefined {
  return history?.messages.findLast(message => message.author === "agent")?.body;
}

/**
 * The overview's attention queue: Threads that need the Owner first (longest
 * wait leading), then the ones currently working, then whatever was active
 * most recently. Archived Threads never appear; snoozed ones only return when
 * their snooze has expired.
 */
/** Every Thread waiting on the Owner right now, longest wait leading. The
 * tree's "Needs you" band, the header pill and the overview queue all read
 * this, so the count and the membership are one fact. */
export function attentionThreads(threads: Thread[], now: number): Thread[] {
  return threads.filter(thread => needsOwner(thread, now))
    .sort((a, b) => (Date.parse(a.last_activity_at) || 0) - (Date.parse(b.last_activity_at) || 0));
}

/**
 * The overview's attention queue: Threads that need the Owner first (longest
 * wait leading), then the ones currently working, then those with activity
 * the Owner has not seen. Archived Threads never appear; snoozed ones only
 * return when their snooze has expired. Waiting Threads are never dropped by
 * `limit` — the tail is what gets trimmed.
 */
export function attentionQueue(
  threads: Thread[],
  histories: Record<number, ThreadHistory | undefined>,
  now: number,
  connected = true,
  limit = 12,
): AttentionEntry[] {
  const since = (thread: Thread) => Date.parse(thread.last_activity_at) || 0;
  const waiting = attentionThreads(threads, now);
  const claimed = new Set(waiting.map(thread => thread.id));
  const eligible = threads.filter(thread => !claimed.has(thread.id)
    && !thread.archived_at
    && !(thread.snoozed_until && Date.parse(thread.snoozed_until) > now)
    && !(thread.kind === "task" && thread.settled_at));
  const busy = eligible.filter(thread => thread.running_turn || thread.queued_turn_count > 0).sort((a, b) => since(b) - since(a));
  const running = new Set(busy.map(thread => thread.id));
  /* "Recent" is news, not the inventory: a Thread with activity the Owner has
     not seen yet. One they have already read stands in the tree beside the
     queue, and repeating it there made the overview a copy of the tree. */
  const rest = eligible.filter(thread => !running.has(thread.id) && !thread.read).sort((a, b) => since(b) - since(a));
  const entry = (thread: Thread, group: AttentionGroup): AttentionEntry => ({
    thread,
    group,
    waited: elapsedTime(threadStatus(thread, now, connected).timestamp ?? thread.last_activity_at, now),
    excerpt: group === "attention" ? attentionExcerpt(lastAgentBody(histories[thread.id])) : "",
  });
  return [
    ...waiting.map(thread => entry(thread, "attention")),
    ...[...busy, ...rest].slice(0, Math.max(0, limit - waiting.length)).map(thread => entry(thread, running.has(thread.id) ? "running" : "recent")),
  ];
}
