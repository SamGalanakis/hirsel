/** Exponential backoff for WebSocket reconnects: 1s, 2s, 4s, ... capped at 30s.
 * `attempt` is 0-indexed (0 = first reconnect attempt after the initial drop). */
export const MIN_BACKOFF_MS = 1000;
export const MAX_BACKOFF_MS = 30000;

export function backoffDelayMs(attempt: number): number {
  if (attempt < 0) return MIN_BACKOFF_MS;
  const delay = MIN_BACKOFF_MS * 2 ** attempt;
  return Math.min(delay, MAX_BACKOFF_MS);
}
