import { describe, expect, it } from "vitest";
import {
  distanceFromBottom,
  FOLLOW_THRESHOLD_PX,
  isAtBottom,
  shouldFollow,
  shouldOfferJump,
} from "./scroll";

/** A tall, scrollable container: 1000px of content in a 400px viewport, so the
 * bottom of the scroll range is scrollTop 600. */
const tall = (scrollTop: number) => ({ scrollTop, clientHeight: 400, scrollHeight: 1000 });

describe("scroll follow decision", () => {
  it("measures distance from the bottom of the scroll range", () => {
    expect(distanceFromBottom(tall(600))).toBe(0);
    expect(distanceFromBottom(tall(0))).toBe(600);
    expect(distanceFromBottom(tall(500))).toBe(100);
  });

  it("treats the exact bottom, and a few pixels of slack, as at-bottom", () => {
    expect(isAtBottom(tall(600))).toBe(true);
    expect(isAtBottom(tall(600 - FOLLOW_THRESHOLD_PX))).toBe(true);
  });

  it("releases the pin as soon as the owner scrolls meaningfully up", () => {
    expect(isAtBottom(tall(600 - FOLLOW_THRESHOLD_PX - 1))).toBe(false);
    expect(isAtBottom(tall(0))).toBe(false);
  });

  it("follows only while at the bottom — a scrolled-up reader is never yanked", () => {
    expect(shouldFollow(tall(600))).toBe(true);
    expect(shouldFollow(tall(200))).toBe(false);
  });

  it("counts non-overflowing content as at-bottom", () => {
    // Content shorter than its container: nowhere to scroll, so new messages
    // still 'follow' and no jump affordance is offered.
    const short = { scrollTop: 0, clientHeight: 400, scrollHeight: 200 };
    expect(isAtBottom(short)).toBe(true);
    expect(shouldFollow(short)).toBe(true);
    expect(shouldOfferJump(short)).toBe(false);
  });

  it("offers the jump affordance only when scrolled up in overflowing content", () => {
    expect(shouldOfferJump(tall(600))).toBe(false);
    expect(shouldOfferJump(tall(0))).toBe(true);
  });
});
