// Recoverable "Mark done" (spec item 2). Now that the host has a real
// `reopen_ping` op (see protocol.ts), Mark done sends `resolve_ping`
// IMMEDIATELY — no deferred/debounced send — and recovery is a true server
// operation rather than a cancelled timer. The Ping still flips to Done at once
// via the optimistic `resolve_local` override (reconciled by the host's `done`
// ping_upsert), and a ~5s "Undo" toast offers `reopen_ping` + an optimistic
// un-flip. This is strictly better than the old debounce: the resolve is never
// lost if the app closes mid-window, and Undo works even after the window (via
// the Done card's ⋯ "Reopen", which shares this reopen path).

import { dispatch } from "../store/store";
import { getClient } from "../ws/client";
import { dismissToast, toast } from "./toast";

/** How long the "Undo" toast lingers after a Mark done. The resolve is already
 * committed to the host by then; this is only the recovery-affordance window
 * (Undo stays reachable after it too, via the Done card's ⋯ "Reopen"). */
export const UNDO_WINDOW_MS = 5000;

/** Mark a Ping done: optimistic Done flip + immediate `resolve_ping`, with a
 * ~5s "Undo" toast whose Undo reopens the Ping. The toast auto-dismiss pauses
 * while it is hovered/focused (see toast.ts + Toaster.tsx), so reaching for
 * Undo never loses it. */
export function markDoneWithUndo(pingId: number): void {
  dispatch({ type: "resolve_local", pingId });
  getClient()?.resolvePing(pingId);
  // `toast()` returns the id synchronously; the Undo action closes over it so a
  // tap dismisses the toast at once (rather than waiting out the window).
  const toastId = toast("Marked done", {
    durationMs: UNDO_WINDOW_MS,
    action: { label: "Undo", onClick: () => undoDone(pingId, toastId) },
  });
}

/** Undo tapped inside the window: reopen the Ping on the host and optimistically
 * un-flip it (dropping any still-pending resolve override so a fast undo shows
 * instantly). The Done card's ⋯ "Reopen" shares this reopen path. */
export function undoDone(pingId: number, toastId?: number): void {
  if (toastId !== undefined) dismissToast(toastId);
  reopenPing(pingId);
}

/** Reopen a resolved Ping: send `reopen_ping` and optimistically un-flip it.
 * Shared by the toast's Undo and the Done card's ⋯ "Reopen". The host's
 * `open` ping_upsert is the source of truth; the un-flip only removes a still
 * -pending optimistic resolve override so a just-marked Ping snaps back at once. */
export function reopenPing(pingId: number): void {
  getClient()?.reopenPing(pingId);
  dispatch({ type: "unresolve_local", pingId });
}
