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

export type ThreadRowIndicator = "attention" | "running" | "queued" | "done" | "none";
export type ThreadRowTone = "attention" | "active" | "danger" | "muted";
export interface ThreadRowSummary {
  indicator: ThreadRowIndicator;
  /** The one short state token the dense row shows, or null when the Thread has
   * nothing to say. Two words at most, so a 288px row stays one line. */
  meta: string | null;
  tone: ThreadRowTone;
  /** How long the Thread has been in that state, for the tooltip. */
  age: string | null;
  sentence: string;
}
/** When a snoozed Thread wakes, in the shortest honest form. */
export function snoozeWakeLabel(thread: Thread, now: number): string | null {
  if (!thread.snoozed_until) return null;
  const wake = Date.parse(thread.snoozed_until);
  if (!Number.isFinite(wake) || wake <= now) return null;
  const sameDay = new Date(wake).toDateString() === new Date(now).toDateString();
  return sameDay
    ? new Date(wake).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
    : new Date(wake).toLocaleDateString([], { month: "short", day: "numeric" });
}
/** One row, one glance: the dense inventory shows a single leading indicator and
 * the row's computed STATE — running, queued n, failed, snoozed until, needs
 * you — never bare recency, which moves into the tooltip beside the full
 * sentence for pointers and into the aria-label for screen readers. */
export function threadRowSummary(thread: Thread, now: number, connected: boolean): ThreadRowSummary {
  const status = threadStatus(thread, now, connected);
  const done = thread.kind === "task" && !!thread.settled_at;
  const wake = snoozeWakeLabel(thread, now);
  const snoozed = !thread.archived_at && wake !== null;
  const indicator: ThreadRowIndicator = thread.attention === "needs_owner" ? "attention"
    : status.state === "running" ? "running"
    : status.state === "queued" ? "queued"
    : done ? "done" : "none";
  const running = status.state === "running" && thread.running_turn?.started_at ? elapsedTime(thread.running_turn.started_at, now) : null;
  const age = !connected ? null : running ?? status.age;
  const state = (): { meta: string | null; tone: ThreadRowTone } => {
    if (!connected) return { meta: null, tone: "muted" };
    if (thread.attention === "needs_owner") return { meta: "needs you", tone: "attention" };
    if (status.state === "running") return { meta: "running", tone: "active" };
    if (thread.queued_turn_count > 0) return { meta: `queued ${thread.queued_turn_count}`, tone: "muted" };
    if (status.state === "failed") return { meta: "failed", tone: "danger" };
    if (status.state === "interrupted" || status.state === "cancelled") return { meta: "stopped", tone: "muted" };
    if (snoozed) return { meta: `until ${wake}`, tone: "muted" };
    if (done) return { meta: "done", tone: "muted" };
    return { meta: age, tone: "muted" };
  };
  const parts = [
    thread.attention === "needs_owner" ? "Needs you" : null,
    showThreadTurnStatus(thread, status.state, true) ? status.label : null,
    done ? "Done" : null,
    thread.archived_at ? "Archived" : null,
    snoozed ? `Snoozed until ${wake}` : null,
  ].filter((part): part is string => !!part);
  return { indicator, ...state(), age, sentence: parts.join(" \u00b7 ") };
}
