import { buildTimeline, splitStreamingReply } from "../components/chat/timeline";
import type { ChatMessage } from "../protocol";
import type { TimelineEvent } from "../store/types";
import { ownerFacingActivity } from "./conversation";
import type { ThreadActivity, ThreadTurn } from "./types";

/**
 * What started this run. The four shapes are exact wire facts, never guesses:
 * a brief from another Thread (`requester_thread_id`), a process wake (the
 * triggering message carries its process origin, or the turn recorded a
 * process activity), an Owner message, and — when a turn has none of those —
 * background execution the Host started on its own.
 */
export type RunOrigin =
  | { kind: "owner" }
  | { kind: "process"; name: string | null }
  | { kind: "delegation"; threadId: number }
  | { kind: "background" };

function processName(activity: ThreadActivity): string | null {
  const data = activity.data !== null && typeof activity.data === "object" && !Array.isArray(activity.data) ? activity.data as Record<string, unknown> : {};
  const payload = data.payload !== null && typeof data.payload === "object" && !Array.isArray(data.payload) ? data.payload as Record<string, unknown> : {};
  const name = data.name ?? data.process ?? payload.name ?? payload.process;
  return typeof name === "string" && name.length > 0 ? name : null;
}

export function runOrigin(turn: ThreadTurn | undefined, trigger: ChatMessage | undefined, activities: ThreadActivity[]): RunOrigin {
  if (turn?.requester_thread_id !== null && turn?.requester_thread_id !== undefined) return { kind: "delegation", threadId: turn.requester_thread_id };
  if (trigger?.origin?.kind === "process") return { kind: "process", name: trigger.origin.name };
  const wake = activities.find(activity => activity.kind.includes("process"));
  if (wake) return { kind: "process", name: processName(wake) };
  // The triggering message can be outside the loaded page; its exact ID on the
  // turn is what says an Owner asked for this run.
  if (trigger || turn?.owner_message_id !== null) return { kind: "owner" };
  return { kind: "background" };
}

/** What to say about where the run came from, in plain words. An Owner message
 * is the default origin and says nothing, so it gets no words at all. */
export function runOriginLabel(origin: RunOrigin): string | null {
  switch (origin.kind) {
    case "owner": return null;
    case "process": return origin.name ? `${origin.name} woke this` : "Woken by a process";
    case "delegation": return `Report from #${origin.threadId}`;
    case "background": return "Background";
  }
}

/**
 * How the run ended, as one word. `quiet` is a completed turn that left the
 * Owner nothing to read: it finished, and saying "Done" over an empty card
 * would claim more than happened.
 */
export type RunOutcome = "queued" | "running" | "done" | "quiet" | "failed" | "cancelled" | "interrupted";

export function runOutcome(turn: ThreadTurn | undefined, message: ChatMessage | undefined, events: TimelineEvent[]): RunOutcome {
  switch (turn?.state) {
    case "queued": return "queued";
    case "running": return "running";
    case "failed": return "failed";
    case "cancelled": return "cancelled";
    case "interrupted": return "interrupted";
    default: break;
  }
  const said = (message?.body ?? splitStreamingReply(events).reply).trim().length > 0 || (message?.artifact_ids?.length ?? 0) > 0;
  return said ? "done" : "quiet";
}

export function runOutcomeLabel(outcome: RunOutcome): string {
  switch (outcome) {
    case "queued": return "queued";
    case "running": return "running";
    case "done": return "done";
    case "quiet": return "quiet";
    case "failed": return "failed";
    case "cancelled": return "cancelled";
    case "interrupted": return "interrupted";
  }
}

/**
 * Every artifact this run produced, once. ADR 0017 publication creates one
 * receipt message carrying the artifact reference; the turn's final reply does
 * not copy it. The defensive join still handles repeated references in legacy
 * frames or activities by keeping the first position. An activity the
 * conversation renders in its own right (a child report, a brief) keeps its
 * cards there; the card never shows the same artifact from both places.
 */
export function turnArtifactIds(message: ChatMessage | undefined, activities: ThreadActivity[]): number[] {
  const seen = new Set<number>();
  const ids: number[] = [];
  const own = activities.filter(activity => !ownerFacingActivity(activity));
  for (const id of [...(message?.artifact_ids ?? []), ...own.flatMap(activity => activity.artifact_ids)]) {
    if (seen.has(id)) continue;
    seen.add(id);
    ids.push(id);
  }
  return ids;
}

/** A run with nothing in its trace still has its identity to show; this is what
 * decides whether the trace holds recorded work as well. */
export function hasTrace(events: TimelineEvent[], activities: ThreadActivity[]): boolean {
  return buildTimeline(events).length > 0 || activities.length > 0;
}
