/**
 * The one elapsed-time vocabulary in the conversation, copied from t3code's
 * `formatDuration` (packages/shared/src/orchestrationTiming.ts): sub-second in
 * milliseconds, one decimal under ten seconds, whole seconds under a minute,
 * and space-joined `h m s` above it with every zero part dropped.
 *
 * It replaced two near-formats that disagreed in the same card — a step row
 * saying `2m20s` above a header saying `2m 20s` — and it never spends more
 * precision than the eye reads at a glance. Callers render it as meta type with
 * `tabular-nums`, never at body size.
 */
export function formatDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return "";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  const seconds = ms / 1000;
  if (seconds < 10) {
    const rounded = seconds.toFixed(1);
    return rounded === "10.0" ? "10s" : `${rounded}s`;
  }
  return formatSeconds(Math.round(seconds));
}

/** The same vocabulary for a duration already measured in whole seconds. */
export function formatSeconds(totalSeconds: number): string {
  if (!Number.isFinite(totalSeconds) || totalSeconds < 0) return "";
  const seconds = Math.floor(totalSeconds);
  if (seconds < 60) return `${seconds}s`;
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const rest = seconds % 60;
  // Zero parts are dropped, so a run that lands on the minute reads `2m`
  // rather than `2m 0s` — one fewer number on the line for no lost meaning.
  return [hours && `${hours}h`, minutes && `${minutes}m`, rest && `${rest}s`].filter(Boolean).join(" ");
}
