// Optimistic decide→resolve with undo for the typed event queue (ADR-0012),
// Deciding a judgment (choosing an
// option, submitting fields, or accepting the recommendation) posts an
// `event_action` to the host IMMEDIATELY and flips the card to decided at once
// via the optimistic `event_decide_local` override (reconciled by the host's
// `done` event_upsert). A ~5s "Undo" toast offers recovery: it re-opens the
// event on the host via an `event_action{action:"reopen"}` (the Done-as-toggle
// peer of a decide) and optimistically un-flips it. Reopen wiring is the same
// recovery machinery Pings use; the host side lands with the event cutover.

import { dispatch } from "../store/store";
import { getClient } from "../ws/client";
import { dismissToast, toast } from "./toast";

/** How long the "Undo" toast lingers after a decide. The action is already sent
 * to the host; this is only the recovery window (the toast auto-dismiss pauses
 * while hovered/focused, so reaching for Undo never loses it). */
export const DECIDE_UNDO_WINDOW_MS = 5000;

/** The structured payload posted back to the producer for a decide. Kept as one
 * function so the sender, the tests, and the on-card "decided" strip agree on
 * the exact shape: `{type:"event_action", event_id, action, data}`. */
export interface DecidePayload {
  type: "event_action";
  event_id: number;
  action: string;
  data: unknown;
}

export function decidePayload(eventId: number, action: string, data: unknown): DecidePayload {
  return { type: "event_action", event_id: eventId, action, data };
}

/** Decide a judgment: optimistic decide flip + immediate `event_action`, with a
 * ~5s "Undo" toast whose Undo reopens the event. `label` is a short human string
 * for the toast (e.g. the chosen option's label). Returns the payload sent, so
 * callers (and tests) can assert or surface it.
 *
 * `silent` suppresses the toast — the scroller passes it because its decided
 * card carries its own on-screen "Undo" strip, so raising a toast on top would
 * double the Undo affordance (§6, "one Undo"). The scroller re-raises a toast
 * itself only once that card scrolls off, keeping recovery reachable without ever
 * showing two Undos at once. */
export function decideEventWithUndo(
  eventId: number,
  action: string,
  data: unknown,
  label?: string,
  opts?: { silent?: boolean },
): DecidePayload {
  dispatch({ type: "event_decide_local", eventId });
  getClient()?.sendEventAction(eventId, action, data);
  if (!opts?.silent) {
    const toastId = toast(label ? `Decided · ${label}` : "Decided", {
      durationMs: DECIDE_UNDO_WINDOW_MS,
      action: { label: "Undo", onClick: () => undoDecide(eventId, toastId) },
    });
  }
  return decidePayload(eventId, action, data);
}

/** Undo tapped inside the window (or the Done card's "Reopen"): reopen the event
 * on the host and optimistically un-flip it. */
export function undoDecide(eventId: number, toastId?: number): void {
  if (toastId !== undefined) dismissToast(toastId);
  reopenEvent(eventId);
}

/** Reopen a decided event: post `event_action{action:"reopen"}` and drop the
 * optimistic decide override so a just-decided card snaps back at once. The
 * host's `open` event_upsert is the source of truth. */
export function reopenEvent(eventId: number): void {
  getClient()?.sendEventAction(eventId, "reopen", null);
  dispatch({ type: "event_undecide_local", eventId });
}

/** Awareness auto-read: flip `read` locally for an instant chip, AND round-trip
 * it on the wire — events share the ping id space, so `read_ping` is the read
 * op. Without the wire half the flag lived in client RAM only: every `hello_ok`
 * full replace reverted the chip to "new" and a second device never learned.
 * The host broadcasts the updated event back, reconciling the local flip. */
export function markEventRead(eventId: number): void {
  dispatch({ type: "event_read_local", eventId });
  getClient()?.readEvent(eventId);
}
