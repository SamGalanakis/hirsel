// Bounded pending state for optimistic controls.
//
// Every "I sent it, now wait" control in the app has the same failure mode: the
// settle signal is not guaranteed to arrive. A host broadcasts a change frame
// only when the value ACTUALLY changed (a no-op write echoes nothing) and an
// error frame returns before any broadcast — so a control that waits for an
// exact-match echo can stay disabled forever. Every pending key here is
// therefore bounded: it settles on the caller's own signal (a matching
// broadcast, an error) OR on a timeout, whichever comes first. A pending key
// never latches.
import { createSignal, onCleanup } from "solid-js";

/** How long a control may show its pending/disabled state without hearing
 * anything back. Long enough that the normal round-trip settles it first, short
 * enough that a lost settle is a blip rather than a stuck surface. */
export const PENDING_MS = 4000;

export interface PendingKeys {
  /** Is this key still awaiting a settle? */
  isPending: (key: string) => boolean;
  /** Is ANY key pending? */
  any: () => boolean;
  /** Mark a key pending and (re)arm its timeout. */
  begin: (key: string) => void;
  /** Settle one key now (matching broadcast, error frame, or caller's choice). */
  settle: (key: string) => void;
  /** Settle every key now — for a signal that is authoritative for all of them
   * (a catalog-wide broadcast, or an error frame that killed the batch). */
  settleAll: () => void;
}

/** A set of independently-tracked pending keys, each bounded by `ms`. */
export function createPendingKeys(ms: number = PENDING_MS): PendingKeys {
  const [keys, setKeys] = createSignal<ReadonlySet<string>>(new Set());
  const timers = new Map<string, ReturnType<typeof setTimeout>>();

  function clearTimer(key: string) {
    const timer = timers.get(key);
    if (timer !== undefined) {
      clearTimeout(timer);
      timers.delete(key);
    }
  }

  function clearAllTimers() {
    for (const timer of timers.values()) clearTimeout(timer);
    timers.clear();
  }

  onCleanup(clearAllTimers);

  const settle = (key: string) => {
    clearTimer(key);
    setKeys((prev) => {
      if (!prev.has(key)) return prev;
      const next = new Set(prev);
      next.delete(key);
      return next;
    });
  };

  const settleAll = () => {
    clearAllTimers();
    setKeys((prev) => (prev.size === 0 ? prev : new Set<string>()));
  };

  const begin = (key: string) => {
    clearTimer(key);
    timers.set(
      key,
      setTimeout(() => settle(key), ms),
    );
    setKeys((prev) => (prev.has(key) ? prev : new Set(prev).add(key)));
  };

  return {
    isPending: (key) => keys().has(key),
    any: () => keys().size > 0,
    begin,
    settle,
    settleAll,
  };
}

/** The single-control case: a local "just submitted" flag that auto-clears
 * after `ms` (and on unmount), used to disable + spin one control briefly. */
export function createSubmitting(ms: number = PENDING_MS): {
  pending: () => boolean;
  begin: () => void;
} {
  const keys = createPendingKeys(ms);
  return { pending: () => keys.any(), begin: () => keys.begin("") };
}
