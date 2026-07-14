// Pure snooze-preset computation (Wave-3 durable snooze). The preset chooser
// (card ⋯ / swipe-left / ⌘K) offers a handful of calm, human return times; this
// module turns "now" into the concrete RFC3339 instants the `event_action
// {snooze,{until}}` frame carries, so the chooser UI, the ⌘K sub-entries, and
// the tests all agree on when each preset lands. `now` is injectable so the
// times are deterministic under test.
//
// Local wall-clock is the source of truth (a person's "this evening" is their
// evening, not UTC's); the instants are serialized with `toISOString()` (Z) for
// the wire.

/** One offered snooze target: a stable `key`, its human `label`, and the
 * resolved RFC3339 `until`. */
export interface SnoozePreset {
  key: string;
  label: string;
  until: string;
}

/** The hour (local) each preset lands on. Evening is 6pm; morning is 9am. */
const EVENING_HOUR = 18;
const MORNING_HOUR = 9;

/** Build a local instant `addDays` from `base`'s day at `hour:00`. */
function atLocalHour(base: Date, addDays: number, hour: number): Date {
  const d = new Date(base.getFullYear(), base.getMonth(), base.getDate() + addDays, hour, 0, 0, 0);
  return d;
}

/** The three quick presets for `now`: "This evening" (today 6pm, or tomorrow 6pm
 * if the evening has already passed), "Tomorrow morning" (tomorrow 9am), and
 * "Next week" (7 days out, 9am). The chooser adds a fourth "Pick time…" entry
 * itself (a free datetime), which is not a fixed preset. */
export function snoozePresets(now: number = Date.now()): SnoozePreset[] {
  const base = new Date(now);
  const eveningToday = atLocalHour(base, 0, EVENING_HOUR);
  const evening = eveningToday.getTime() > now ? eveningToday : atLocalHour(base, 1, EVENING_HOUR);
  return [
    { key: "evening", label: "This evening", until: evening.toISOString() },
    { key: "tomorrow", label: "Tomorrow morning", until: atLocalHour(base, 1, MORNING_HOUR).toISOString() },
    { key: "week", label: "Next week", until: atLocalHour(base, 7, MORNING_HOUR).toISOString() },
  ];
}

/** Convert a `datetime-local` input value (local wall-clock, no zone) to the
 * RFC3339 (Z) instant the wire wants. Returns null for an empty/invalid value so
 * the caller can keep the chooser open rather than send an invalid snooze. */
export function datetimeLocalToIso(value: string): string | null {
  if (!value) return null;
  const ms = Date.parse(value); // a datetime-local string parses as local time
  if (!Number.isFinite(ms)) return null;
  return new Date(ms).toISOString();
}
