import { describe, expect, it } from "vitest";
import { computeEdges, isNearBottom } from "./message-scroller";

describe("MessageScroller edge math (near-bottom autoscroll)", () => {
  it("reports atEnd when within the threshold of the bottom", () => {
    // scrollHeight 1000, viewport 400, scrolled to 590 -> 10px from bottom.
    const edges = computeEdges(590, 1000, 400);
    expect(edges.distanceFromBottom).toBe(10);
    expect(edges.atEnd).toBe(true);
    expect(edges.atStart).toBe(false);
  });

  it("reports NOT atEnd once scrolled well above the bottom", () => {
    const edges = computeEdges(100, 1000, 400); // 500px from bottom
    expect(edges.atEnd).toBe(false);
    expect(edges.atStart).toBe(false);
  });

  it("reports atStart at the very top", () => {
    const edges = computeEdges(0, 1000, 400);
    expect(edges.atStart).toBe(true);
    expect(edges.atEnd).toBe(false);
  });

  it("treats a non-scrollable viewport as both ends", () => {
    // Content shorter than the viewport: nothing to scroll -> already at end.
    const edges = computeEdges(0, 300, 400);
    expect(edges.atStart).toBe(true);
    expect(edges.atEnd).toBe(true);
  });

  it("honours a custom threshold", () => {
    expect(computeEdges(880, 1000, 100, 4).atEnd).toBe(false); // 20px gap > 4
    expect(computeEdges(880, 1000, 100, 32).atEnd).toBe(true); // 20px gap <= 32
  });

  it("isNearBottom mirrors the atEnd decision from a DOM-ish element", () => {
    expect(isNearBottom({ scrollTop: 590, scrollHeight: 1000, clientHeight: 400 })).toBe(true);
    expect(isNearBottom({ scrollTop: 100, scrollHeight: 1000, clientHeight: 400 })).toBe(false);
  });
});
