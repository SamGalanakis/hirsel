/** Human-readable byte size for attachment chips (e.g. "1.4 MB", "820 KB"). */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "";
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}

/** Compact relative-time label like "just now", "3m ago", "2h ago", "4d ago"
 * for process rows. `now` is injectable for deterministic tests. */
export function formatRelativeTime(ts: string, now: number = Date.now()): string {
  const then = Date.parse(ts);
  if (!Number.isFinite(then)) return "";
  const secs = Math.max(0, Math.round((now - then) / 1000));
  if (secs < 45) return "just now";
  const mins = Math.round(secs / 60);
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.round(hours / 24);
  return `${days}d ago`;
}

/** Compact resting-row timestamp (spec item 5): a terse relative label for
 * recent Pings — "now" (<1m), "5m" (<1h), "3h" (<24h) — or null past 24h so
 * the caller falls back to its absolute date form. Terser than
 * `formatRelativeTime` (no "ago") because it rests beside a title on a dense
 * row where quietness matters. `now` is injectable for deterministic tests. */
export function relativePingTime(ts: string, now: number = Date.now()): string | null {
  const then = Date.parse(ts);
  if (!Number.isFinite(then)) return null;
  const secs = Math.max(0, Math.round((now - then) / 1000));
  if (secs < 60) return "now";
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}m`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h`;
  return null;
}

/** Compact event-card age (Wave-3 time axis): the terse relative label for a
 * recent event — "2m", "6h" (via `relativePingTime`) — or, past 24h, a short
 * absolute date ("Jul 13"). Quiet meta, tabular-nums at the call site. Returns ""
 * for an unparseable ts so the header simply omits the age. `now` is injectable
 * for deterministic tests. */
export function formatEventAge(ts: string, now: number = Date.now()): string {
  const rel = relativePingTime(ts, now);
  if (rel !== null) return rel;
  const then = Date.parse(ts);
  if (!Number.isFinite(then)) return "";
  return new Date(then).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

/** The return-time label for a snoozed row (Wave-3 durable snooze): when the
 * Event comes back, as a compact wall-clock — "Today 6:00 PM", "Tomorrow 9:00
 * AM", a weekday within the week ("Mon 9:00 AM"), else a short date ("Jul 21").
 * Local time; tabular-nums at the call site. Returns "" for an unparseable
 * instant. `now` is injectable for deterministic tests. */
export function formatReturnTime(until: string, now: number = Date.now()): string {
  const then = Date.parse(until);
  if (!Number.isFinite(then)) return "";
  const target = new Date(then);
  const time = target.toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
  const startOfDay = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const dayDiff = Math.round((startOfDay(target) - startOfDay(new Date(now))) / 86_400_000);
  if (dayDiff <= 0) return `Today ${time}`;
  if (dayDiff === 1) return `Tomorrow ${time}`;
  if (dayDiff < 7) {
    return `${target.toLocaleDateString(undefined, { weekday: "short" })} ${time}`;
  }
  return target.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

/** The calendar day-group label for the archived day-log (Wave-3 time axis):
 * "Today", "Yesterday", else a full date ("Jul 13, 2026"). Groups the Archived
 * view by the day an event was swept. `now` is injectable for deterministic
 * tests. */
export function formatDayGroup(ts: string, now: number = Date.now()): string {
  const then = Date.parse(ts);
  if (!Number.isFinite(then)) return "Earlier";
  const target = new Date(then);
  const startOfDay = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const dayDiff = Math.round((startOfDay(new Date(now)) - startOfDay(target)) / 86_400_000);
  if (dayDiff <= 0) return "Today";
  if (dayDiff === 1) return "Yesterday";
  return target.toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" });
}

/** One-line, whitespace-collapsed, ellipsis-truncated preview of a body of
 * text — shared by the Inbox card's reply quote, the Processes "Ask to stop"
 * pre-fill. */
export function snippet(body: string, maxLen = 80): string {
  const oneLine = body.replace(/\s+/g, " ").trim();
  return oneLine.length > maxLen ? `${oneLine.slice(0, maxLen)}…` : oneLine;
}

/** Read a File as a bare base64 string (no data: prefix) for upload_blob. */
export function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const result = reader.result as string;
      const comma = result.indexOf(",");
      resolve(comma === -1 ? result : result.slice(comma + 1));
    };
    reader.onerror = () => reject(reader.error ?? new Error("file read failed"));
    reader.readAsDataURL(file);
  });
}
