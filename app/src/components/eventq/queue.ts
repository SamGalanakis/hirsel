// Pure paging helpers for the event scroller, split out so the decide→advance
// and auto-read logic is unit-testable without driving scroll in jsdom (where
// clientHeight is 0). The scroller wires these to real scroll positions.
import type { EventItem } from "../../protocol";
import { isEventResolved } from "../../store/selectors";

/** The index of the next page to advance to after deciding the event at `from`,
 * within an ORDERED event list (see selectors.orderedQueue). Skips
 * already-resolved judgments; lands on the first later unresolved event, else on
 * the clear page (index === events.length) when nothing is left. */
export function nextOpenIndex(ordered: EventItem[], from: number, decideOverrides: number[]): number {
  for (let i = from + 1; i < ordered.length; i++) {
    const e = ordered[i];
    if (e.kind === "judgment" && isEventResolved(e, decideOverrides)) continue;
    return i;
  }
  return ordered.length; // the clear page
}

/** Awareness (summary/info) events strictly BEFORE `current` in the ordered list
 * that are still unread — the set the scroller auto-marks-read as they scroll
 * past (never while centred). */
export function awarenessToAutoRead(ordered: EventItem[], current: number): EventItem[] {
  const out: EventItem[] = [];
  for (let i = 0; i < current && i < ordered.length; i++) {
    const e = ordered[i];
    if (e.kind !== "judgment" && !e.read) out.push(e);
  }
  return out;
}
