// The email-like "seen" decision, factored out of the Inbox card so the timing
// logic is unit-testable with fake timers (the IntersectionObserver wiring that
// drives it lives in the component). An item counts as "seen" once it has been
// continuously visible for SEEN_DELAY_MS, or immediately on first interaction
// (expanding the reply input, tapping a quick reply). onSeen fires at most once.

export const SEEN_DELAY_MS = 1500;

export interface SeenTimer {
  /** Report whether the card is currently visible in the viewport. Becoming
   * visible arms the delay; becoming hidden before it elapses cancels it. */
  setVisible(visible: boolean): void;
  /** The Owner interacted with the card — counts as seen immediately. */
  interacted(): void;
  /** Tear down any pending timer (component cleanup). */
  dispose(): void;
}

/** Create a one-shot "seen" latch. Uses the ambient setTimeout/clearTimeout so
 * `vi.useFakeTimers()` can drive it directly in tests. */
export function createSeenTimer(opts: { onSeen: () => void; delayMs?: number }): SeenTimer {
  const delayMs = opts.delayMs ?? SEEN_DELAY_MS;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let fired = false;

  function cancel(): void {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  }

  function fire(): void {
    if (fired) return;
    fired = true;
    cancel();
    opts.onSeen();
  }

  return {
    setVisible(visible: boolean): void {
      if (fired) return;
      if (visible) {
        if (timer === null) timer = setTimeout(fire, delayMs);
      } else {
        cancel();
      }
    },
    interacted(): void {
      fire();
    },
    dispose(): void {
      cancel();
    },
  };
}
