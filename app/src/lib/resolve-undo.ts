// Recoverable "Mark done" (spec item 2). `resolve_ping` is terminal on the wire
// — there is no reopen/unresolve op (see protocol.ts) — so a mis-tap or
// mis-swipe would be irreversible. Instead of inventing a host op, Mark done is
// made recoverable client-side, modelled on the existing optimistic read-flip
// machinery: the Ping flips to Done at once (an optimistic `resolve_local`
// override), a 5s "Undo" toast debounces the actual `resolve_ping` send, and
// Undo drops the override before anything reaches the host. Only once the window
// elapses is the resolve committed; the host's `done` ping_upsert then supersedes
// the override (the reducer prunes it).

import { dispatch } from "../store/store";
import { getClient } from "../ws/client";
import { dismissToast, toast } from "./toast";

/** How long the resolve send is held so a mis-tap/mis-swipe is recoverable. */
export const UNDO_WINDOW_MS = 5000;

interface Pending {
  timer: ReturnType<typeof setTimeout>;
  toastId: number;
}

// One pending resolve per Ping id: the debounce timer plus the toast to dismiss
// if Undo is taken before it fires.
const pending = new Map<number, Pending>();

/** Mark a Ping done, recoverably: optimistic Done flip now, `resolve_ping`
 * debounced behind a 5s "Undo" toast. Idempotent while a window is open (a
 * second tap keeps the first timer rather than resetting it). */
export function markDoneWithUndo(pingId: number): void {
  if (pending.has(pingId)) return;
  dispatch({ type: "resolve_local", pingId });
  const toastId = toast("Marked done", {
    durationMs: UNDO_WINDOW_MS,
    action: { label: "Undo", onClick: () => undoDone(pingId) },
  });
  const timer = setTimeout(() => commitDone(pingId), UNDO_WINDOW_MS);
  pending.set(pingId, { timer, toastId });
}

/** Undo tapped inside the window: cancel the pending send, dismiss the toast,
 * and drop the optimistic override so the Ping returns to open. */
export function undoDone(pingId: number): void {
  const entry = pending.get(pingId);
  if (entry) {
    clearTimeout(entry.timer);
    dismissToast(entry.toastId);
    pending.delete(pingId);
  }
  dispatch({ type: "unresolve_local", pingId });
}

/** The window elapsed with no Undo: commit the resolve to the host. The override
 * lingers until the host's `done` ping_upsert lands (the reducer prunes it then),
 * so the card never flickers back to open in the gap. */
function commitDone(pingId: number): void {
  pending.delete(pingId);
  getClient()?.resolvePing(pingId);
}

/** Test seam: are there any un-committed pending resolves? */
export function hasPendingResolve(pingId: number): boolean {
  return pending.has(pingId);
}
