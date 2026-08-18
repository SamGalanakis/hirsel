// Optimistic durable Task snooze/unsnooze (typed Event wire contract), the
// snooze twin of lib/event-archive.ts. Snoozing an event posts
// `event_action{snooze,{until}}` to the host IMMEDIATELY and lifts the card out
// of Active at once via the optimistic `event_snooze_local` flip (which patches
// `snoozed_until` on the event; reconciled by the host's echo, and later cleared
// by the host's timer at the return instant — the return IS a fresh interrupt).
// Snooze is a QUIET verb (DESIGN.md): no confirmation, nothing red — a calm
// toast offers the immediate Undo, and the "Snoozed (n)" filter's Unsnooze is
// the durable path.

import { dispatch } from "../store/store";
import { getClient } from "../ws/client";
import { dismissToast, toast } from "./toast";

/** The exact wire envelope for a snooze: `data` carries the RFC3339 return
 * instant — `{"type":"event_action","event_id":N,"action":"snooze","data":{"until":"…"}}`.
 * A snooze without `until` is invalid (the host rejects it), so this shape is the
 * single source of the field. */
export interface SnoozePayload {
  type: "event_action";
  event_id: number;
  action: "snooze" | "unsnooze";
  data: { until: string } | Record<string, never>;
}

export function snoozePayload(eventId: number, until: string): SnoozePayload {
  return { type: "event_action", event_id: eventId, action: "snooze", data: { until } };
}

/** Snooze an event until `until` (RFC3339): optimistic lift + immediate
 * `event_action{snooze}`, with a quiet "Snoozed" toast whose Undo un-snoozes.
 * `label` is a short human string for the toast (e.g. the return time). Returns
 * the payload sent so callers (and tests) can assert it. */
export function snoozeEventWithUndo(
  eventId: number,
  until: string,
  label?: string,
  opts?: { silent?: boolean },
): SnoozePayload {
  dispatch({ type: "event_snooze_local", eventId, until });
  getClient()?.sendEventAction(eventId, "snooze", { until });
  if (!opts?.silent) {
    const toastId = toast(label ? `Snoozed · ${label}` : "Snoozed", {
      action: {
        label: "Undo",
        onClick: () => {
          dismissToast(toastId);
          unsnoozeEvent(eventId);
        },
      },
    });
  }
  return snoozePayload(eventId, until);
}

/** Un-snooze (the "Snoozed (n)" row action, or the toast Undo): optimistic
 * return to Active + `event_action{unsnooze}`. */
export function unsnoozeEvent(eventId: number): SnoozePayload {
  dispatch({ type: "event_unsnooze_local", eventId });
  getClient()?.sendEventAction(eventId, "unsnooze", {});
  return { type: "event_action", event_id: eventId, action: "unsnooze", data: {} };
}
