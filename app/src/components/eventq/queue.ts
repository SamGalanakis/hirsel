// Pure paging helpers for the event scroller, split out so the decide→advance
// and auto-read logic is unit-testable without driving scroll in jsdom (where
// clientHeight is 0). The scroller wires these to real scroll positions.
import type { EventItem } from "../../protocol";
import { isEventResolved, isOpenJudgment } from "../../store/selectors";

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

/** The index the pager should anchor to when it (re)gains an event set: the
 * first still-open judgment in the ORDERED list (the first needs-you card), else
 * 0. Used on mount and whenever a `hello_ok` snapshot replaces the set — the
 * viewport must land on what needs the Owner, never wherever the browser's
 * preserved scroll offset / snap tracking happened to leave it. */
export function firstOpenIndex(ordered: EventItem[], decideOverrides: number[]): number {
  const idx = ordered.findIndex((e) => isOpenJudgment(e, decideOverrides));
  return idx === -1 ? 0 : idx;
}

/** Whether navigating OFF a page should auto-mark its event read: awareness
 * (summary/info) only, still unread. The event must have actually been the
 * viewed page — the caller tracks that by id — so pages skipped by an initial
 * jump or shuffled by a snapshot replacement are never marked (the regression:
 * a browser-driven scroll used to convert into phantom reads). */
export function shouldMarkReadOnLeave(prev: EventItem | undefined | null): prev is EventItem {
  return prev != null && prev.kind !== "judgment" && !prev.read;
}
