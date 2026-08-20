import type { ScrollGeometry } from "./scroll";

export const CONVERSATION_WINDOW = 30;
export const HISTORY_PAGE_SIZE = 100;
export const PREFETCH_VIEWPORTS = 1.5;

export interface ScrollbackDecisionInput {
  geometry: ScrollGeometry;
  rendered: number;
  loaded: number;
  hasEarlier: boolean;
  backfillInFlight: boolean;
}

export interface ScrollbackDecision {
  reveal: number;
  fetchBeforeOldest: boolean;
}

/** Pure geometry/state decision shared by scroll handling and unit tests. */
export function decideScrollback(input: ScrollbackDecisionInput): ScrollbackDecision {
  const nearTop = input.geometry.scrollTop <= input.geometry.clientHeight * PREFETCH_VIEWPORTS;
  if (!nearTop) return { reveal: 0, fetchBeforeOldest: false };

  const hidden = Math.max(0, input.loaded - input.rendered);
  if (hidden > 0) {
    return {
      reveal: Math.min(CONVERSATION_WINDOW, hidden),
      fetchBeforeOldest: false,
    };
  }

  return {
    reveal: 0,
    fetchBeforeOldest: input.hasEarlier && !input.backfillInFlight,
  };
}

/** The prefetch was outrun only at the actual top edge, not merely in its margin. */
export function outranHistoryPrefetch(geometry: ScrollGeometry): boolean {
  return geometry.scrollTop <= 1;
}

export interface PrependAnchor {
  scrollTop: number;
  scrollHeight: number;
}

export function capturePrependAnchor(element: HTMLElement): PrependAnchor {
  return { scrollTop: element.scrollTop, scrollHeight: element.scrollHeight };
}

/**
 * Preserve the same visual content after rows are inserted above it.
 *
 * Chromium's overflow anchoring normally lands on `expected` already. The
 * tolerance makes that native path a no-op; assigning only when it did not
 * happen avoids double-applying the height delta while keeping Safari/jsdom
 * and layout edge cases deterministic.
 */
export function restorePrependAnchor(
  element: HTMLElement,
  anchor: PrependAnchor,
  tolerance = 1,
): void {
  const growth = element.scrollHeight - anchor.scrollHeight;
  if (growth <= 0) return;
  const expected = anchor.scrollTop + growth;
  if (Math.abs(element.scrollTop - expected) > tolerance) element.scrollTop = expected;
}
