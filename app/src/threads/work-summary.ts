import { buildTimeline, type TimelineItem } from "../components/chat/timeline";
import type { ToolCall } from "../protocol";
import type { TimelineEvent } from "../store/types";
import type { ThreadActivity, ThreadTurn } from "./types";

export function activityData(activity: ThreadActivity): Record<string, unknown> {
  return activity.data !== null && typeof activity.data === "object" && !Array.isArray(activity.data) ? activity.data as Record<string, unknown> : {};
}
export function toolSummary(activity: ThreadActivity): ToolCall | null {
  const data = activityData(activity);
  return activity.kind === "tool_completed" && typeof data.name === "string" && typeof data.ok === "boolean" ? { name: data.name, ok: data.ok } : null;
}
/** A final message/activity can arrive after a reconnect omitted a tool's done
 * event. Complete that exact started row from the persisted name/outcome so one
 * invocation keeps one position and one status. Result text remains absent. */
export function resolveStartedTools(events: TimelineEvent[], calls: ToolCall[]): TimelineEvent[] {
  const items = buildTimeline(events);
  const unmatched = [...calls];
  for (const item of items) {
    if (item.kind !== "tool") continue;
    const status = item.status;
    if (status.state !== "done") continue;
    const index = unmatched.findIndex(call => call.name === item.name && call.ok === status.ok);
    if (index >= 0) unmatched.splice(index, 1);
  }
  let seq = Math.max(0, ...events.map(event => event.seq));
  const completions: TimelineEvent[] = [];
  for (const item of items) {
    if (item.kind !== "tool" || item.status.state !== "running") continue;
    const index = unmatched.findIndex(call => call.name === item.name);
    if (index < 0) continue;
    const [call] = unmatched.splice(index, 1);
    completions.push({ seq: ++seq, event: { kind: "tool_done", id: item.toolId, name: item.name, ok: call.ok, summary: null } });
  }
  return completions.length > 0 ? [...events, ...completions] : events;
}
/** Rich events keep their exact call IDs and order. The persisted name/outcome
 * list fills only missing occurrences when reconnect supplied a partial stream. */
export function remainingTools(calls: ToolCall[], items: TimelineItem[]): ToolCall[] {
  const counts = new Map<string, number>();
  const key = (name: string, ok: boolean) => JSON.stringify([name, ok]);
  for (const item of items) if (item.kind === "tool" && item.status.state === "done") {
    const id = key(item.name, item.status.ok);
    counts.set(id, (counts.get(id) ?? 0) + 1);
  }
  return calls.filter(call => {
    const id = key(call.name, call.ok), count = counts.get(id) ?? 0;
    if (!count) return true;
    counts.set(id, count - 1); return false;
  });
}
export function workDuration(turn: ThreadTurn | undefined, now: number): string {
  if (!turn || turn.state === "queued") return "";
  const end = turn.finished_at ? Date.parse(turn.finished_at) : now;
  const seconds = Math.max(0, Math.floor((end - Date.parse(turn.started_at)) / 1000));
  if (!Number.isFinite(seconds)) return "";
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  return minutes < 60 ? `${minutes}m ${seconds % 60}s` : `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}
function runningTool(name: string): string {
  if (/(exec|bash|shell|command)/i.test(name)) return "Running a command";
  if (/(delegate|spawn)/i.test(name)) return "Delegating work";
  if (/(read|list|search|find)/i.test(name)) return "Gathering context";
  if (/artifact/i.test(name)) return "Working on an artifact";
  if (/(edit|write|patch)/i.test(name)) return "Making changes";
  return `Using ${name.replaceAll("_", " ").replaceAll(".", " · ")}`;
}
export function failureReason(activities: ThreadActivity[]): string | null {
  const failure = activities.findLast(activity => /(^|_)(error|failed)$/.test(activity.kind));
  if (!failure) return null;
  const data = activityData(failure);
  const text = [data.reason, data.message, data.error].find(value => typeof value === "string" && value.trim());
  return typeof text === "string" ? text.replace(/\s+/g, " ").slice(0, 240) : null;
}
export function workLabel(turn: ThreadTurn | undefined, events: TimelineEvent[], activities: ThreadActivity[], count: number, hasReply: boolean): string {
  if (turn?.state === "queued") return "Queued";
  if (turn?.state === "failed" || (!turn && failureReason(activities))) return "Couldn’t finish";
  if (turn?.state === "cancelled") return "Stopped";
  if (turn?.state === "interrupted") return "Interrupted";
  if (turn?.state === "running") {
    const pending = buildTimeline(events).findLast(item => item.kind === "tool" && item.status.state === "running");
    if (pending?.kind === "tool") return runningTool(pending.name);
    const progress = activities.findLast(activity => activity.kind === "execution_progress");
    const summary = progress && activityData(progress).summary;
    if (typeof summary === "string" && summary.trim()) return summary.replace(/\s+/g, " ").slice(0, 120);
    if (events.at(-1)?.event.kind === "prose") return "Writing a reply";
    if (events.at(-1)?.event.kind === "reasoning") return "Thinking";
    return "Hirsel is working…";
  }
  if (count) return `Used ${count} ${count === 1 ? "tool" : "tools"}`;
  if (turn?.state === "completed" && !hasReply) return "Finished without a reply";
  return "Work details";
}
