import { describe, expect, it } from "vitest";
import {
  CONVERSATION_WINDOW,
  capturePrependAnchor,
  decideScrollback,
  outranHistoryPrefetch,
  restorePrependAnchor,
} from "./scrollback";

describe("just-in-time scrollback decisions", () => {
  const geometry = { scrollTop: 700, clientHeight: 500, scrollHeight: 3000 };

  it("reveals one client batch inside the 1.5-viewport prefetch margin", () => {
    expect(decideScrollback({
      geometry,
      rendered: 30,
      loaded: 85,
      hasEarlier: true,
      backfillInFlight: false,
    })).toEqual({ reveal: CONVERSATION_WINDOW, fetchBeforeOldest: false });
  });

  it("requests host history only after loaded rows are all rendered", () => {
    expect(decideScrollback({
      geometry,
      rendered: 85,
      loaded: 85,
      hasEarlier: true,
      backfillInFlight: false,
    })).toEqual({ reveal: 0, fetchBeforeOldest: true });
  });

  it("guards a second host request while one is in flight", () => {
    expect(decideScrollback({
      geometry,
      rendered: 85,
      loaded: 85,
      hasEarlier: true,
      backfillInFlight: true,
    })).toEqual({ reveal: 0, fetchBeforeOldest: false });
  });

  it("does nothing outside the prefetch margin or at true history start", () => {
    expect(decideScrollback({
      geometry: { ...geometry, scrollTop: 751 },
      rendered: 30,
      loaded: 85,
      hasEarlier: true,
      backfillInFlight: false,
    })).toEqual({ reveal: 0, fetchBeforeOldest: false });
    expect(decideScrollback({
      geometry,
      rendered: 85,
      loaded: 85,
      hasEarlier: false,
      backfillInFlight: false,
    })).toEqual({ reveal: 0, fetchBeforeOldest: false });
  });

  it("shows loading only when the reader reaches the actual top edge", () => {
    expect(outranHistoryPrefetch({ ...geometry, scrollTop: 1 })).toBe(true);
    expect(outranHistoryPrefetch({ ...geometry, scrollTop: 2 })).toBe(false);
  });
});

describe("prepend scroll anchoring", () => {
  it("adds scrollHeight growth when native overflow anchoring did not", () => {
    const element = { scrollTop: 240, scrollHeight: 1000 } as HTMLElement;
    const anchor = capturePrependAnchor(element);
    Object.defineProperty(element, "scrollHeight", { value: 1360, configurable: true });

    restorePrependAnchor(element, anchor);
    expect(element.scrollTop).toBe(600);
  });

  it("does not double-apply a native overflow-anchor adjustment", () => {
    const element = { scrollTop: 240, scrollHeight: 1000 } as HTMLElement;
    const anchor = capturePrependAnchor(element);
    Object.defineProperty(element, "scrollHeight", { value: 1360, configurable: true });
    element.scrollTop = 600;

    restorePrependAnchor(element, anchor);
    expect(element.scrollTop).toBe(600);
  });
});
