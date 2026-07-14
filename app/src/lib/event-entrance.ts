// Genuine-arrival tracking for the card entrance animation (craft wave). A card
// should fade+settle in ONLY when it is a genuinely NEW arrival on the wire (a
// live `event_upsert` introducing an id we have not seen) — never when the
// initial `hello_ok` snapshot hydrates the whole queue at once (that would flash
// every card in on load, the opposite of calm). The `event_upsert` path in the
// store marks an arrival; the card consumes its flag once on mount, so a later
// re-render (a decide flip, a resort) never replays the entrance.
//
// Kept as a tiny standalone module (not store state) so it stays a pure UI
// concern: the reducer that owns the protocol slice never has to carry an
// animation flag, and the mark/consume pair is trivially testable in isolation.

const arrivals = new Set<number>();

/** Mark event `id` as a genuine live arrival — its next mount plays the entrance.
 * Called from the store's `event_upsert` path only when the id is genuinely new
 * (not already in the set), never from `hello_ok` hydration. */
export function markArrival(id: number): void {
  arrivals.add(id);
}

/** Consume `id`'s arrival flag: true exactly once, for the card that mounts for
 * a freshly-arrived event. Every other mount (initial hydration, a re-render)
 * returns false, so only genuine arrivals animate. */
export function consumeArrival(id: number): boolean {
  if (!arrivals.has(id)) return false;
  arrivals.delete(id);
  return true;
}

/** Test seam: forget every pending arrival flag. */
export function resetArrivals(): void {
  arrivals.clear();
}
