/** Scroll geometry of a scrollable element, narrowed to the three numbers the
 * follow decision needs. Kept as a plain shape so the decision is testable
 * without a layout engine. */
export interface ScrollGeometry {
  scrollTop: number;
  clientHeight: number;
  scrollHeight: number;
}

/** How far from the bottom still counts as "at the bottom".
 *
 * Not zero: sub-pixel rounding, a fractional device pixel ratio, and the last
 * line's leading all leave a few pixels of slack that the Owner never
 * perceives as having scrolled away. One line of body text (14px × 1.625
 * leading, rounded up) is the honest tolerance — small enough that a
 * deliberate scroll of even one line releases the pin. */
export const FOLLOW_THRESHOLD_PX = 24;

/** Distance from the bottom of the scroll range, in pixels. */
export function distanceFromBottom(geometry: ScrollGeometry): number {
  return geometry.scrollHeight - geometry.scrollTop - geometry.clientHeight;
}

/**
 * The pin/unpin decision, in one place.
 *
 * `true` means new content should keep the view pinned to the bottom. Content
 * that cannot scroll at all (shorter than its container) is always "at the
 * bottom" — there is nowhere to be scrolled away to, and treating it as
 * unpinned would strand a fresh conversation behind a "jump to latest" button
 * pointing at content already fully on screen.
 */
export function isAtBottom(
  geometry: ScrollGeometry,
  threshold: number = FOLLOW_THRESHOLD_PX,
): boolean {
  return distanceFromBottom(geometry) <= threshold;
}

/**
 * Whether a growth event should scroll the view down.
 *
 * The rule the whole feature rests on: follow only while the Owner is already
 * at the bottom. Once they scroll up they are reading, and NOTHING may yank
 * them back — the "jump to latest" affordance is the only way back down, and it
 * is theirs to press.
 */
export function shouldFollow(geometry: ScrollGeometry, threshold?: number): boolean {
  return isAtBottom(geometry, threshold);
}

/** Whether the "jump to latest" affordance should be offered: only when there
 * is somewhere to jump TO (the content actually overflows) and the Owner is not
 * already there. */
export function shouldOfferJump(
  geometry: ScrollGeometry,
  threshold: number = FOLLOW_THRESHOLD_PX,
): boolean {
  const scrollable = geometry.scrollHeight - geometry.clientHeight > threshold;
  return scrollable && !isAtBottom(geometry, threshold);
}

/** `smooth` unless the Owner asked for less motion (DESIGN §5), in which case
 * the jump is instant. Reads the media query at call time so a mid-session
 * preference change is honoured. */
export function scrollBehavior(): ScrollBehavior {
  const reduced =
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  return reduced ? "auto" : "smooth";
}

/** Pin an element to the bottom of its scroll range. `instant` is used for
 * follow-during-stream, where the target moves every frame and an in-flight
 * smooth animation would visibly lag behind the text being written. */
export function scrollToBottom(element: HTMLElement, instant = false): void {
  // `Element.scrollTo` is absent in jsdom and in older engines. Assigning
  // `scrollTop` is the universally supported equivalent (it just cannot be
  // smooth), so the pin still lands rather than throwing out of the animation
  // frame that scheduled it.
  if (typeof element.scrollTo !== "function") {
    element.scrollTop = element.scrollHeight;
    return;
  }
  element.scrollTo({
    top: element.scrollHeight,
    behavior: instant ? "auto" : scrollBehavior(),
  });
}
