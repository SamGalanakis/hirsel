import type { Thread } from "./types";
export function elapsedTime(timestamp: string, now: number): string | null {
  const elapsed = now - Date.parse(timestamp);
  if (!Number.isFinite(elapsed) || elapsed < -60_000) return null;
  const minutes = Math.floor(Math.max(0, elapsed) / 60_000);
  if (minutes < 1) return "<1m";
  if (minutes < 60) return `${minutes}m`;
  if (minutes < 1440) return `${Math.floor(minutes / 60)}h${minutes % 60 ? ` ${minutes % 60}m` : ""}`;
  return `${Math.floor(minutes / 1440)}d`;
}
export function threadStatus(thread: Thread, now: number, connected: boolean) {
  if (typeof thread.queued_turn_count !== "number" || !thread.last_activity_at || thread.running_turn === undefined || thread.last_finished_turn === undefined) return { state: "unavailable", label: "Status unavailable", timestamp: null, age: null, timeLabel: "", stale: true };
  const running = thread.running_turn;
  const queued = thread.queued_turn_count;
  const terminal = thread.last_finished_turn;
  const state = running ? "running" : queued > 0 ? "queued" : terminal?.state ?? "idle";
  const labels = { running: "Working", queued: "Queued", completed: "Turn finished", failed: "Turn failed", interrupted: "Interrupted", cancelled: "Cancelled", idle: "No turns yet" };
  const anchor = running?.started_at ?? terminal?.finished_at ?? thread.last_activity_at;
  const timestamp = Number.isFinite(Date.parse(anchor)) ? anchor : null;
  const age = timestamp && connected ? elapsedTime(timestamp, now) : null;
  const label = labels[state];
  const queue = running && queued > 0 ? ` · ${queued} queued` : state === "queued" && queued > 1 ? ` (${queued})` : "";
  return { state, label: `${connected ? "" : "Last known: "}${label}${state === "running" && age ? ` ${age}` : ""}${queue}`, timestamp,
    age: state === "running" || state === "queued" ? null : age,
    timeLabel: state === "idle" ? "Last conversation activity" : "Turn finished", stale: !connected };
}
export function showThreadTurnStatus(thread: Thread, state: string, compact = false): boolean {
  if (compact && state === "idle") return false;
  return !(thread.kind === "space" && state === "completed");
}
