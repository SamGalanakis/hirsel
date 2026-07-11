/** Exponential backoff for WebSocket reconnects: 1s, 2s, 4s, ... capped at 30s.
 * `attempt` is 0-indexed (0 = first reconnect attempt after the initial drop). */
export const MIN_BACKOFF_MS = 1000;
export const MAX_BACKOFF_MS = 30000;

export function backoffDelayMs(attempt: number): number {
  if (attempt < 0) return MIN_BACKOFF_MS;
  const delay = MIN_BACKOFF_MS * 2 ** attempt;
  return Math.min(delay, MAX_BACKOFF_MS);
}

/** ±20% of uniform jitter on the exponential base, so a fleet of clients that
 * dropped together (host restart) don't all reconnect on the same tick and
 * thundering-herd the host. `rng` is injectable for deterministic tests. */
export function jitteredDelayMs(attempt: number, rng: () => number = Math.random): number {
  const base = backoffDelayMs(attempt);
  // factor ∈ [0.8, 1.2)
  const factor = 0.8 + 0.4 * rng();
  return Math.round(base * factor);
}
