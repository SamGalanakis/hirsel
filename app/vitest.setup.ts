import "@testing-library/jest-dom/vitest";
import { cleanup } from "@solidjs/testing-library";
import { afterEach } from "vitest";

// jsdom lacks Element#scrollIntoView / #scrollTo; ChatView scrolls messages
// into view and the thread to the bottom. No-ops are enough - no layout to
// scroll in jsdom.
if (!Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = () => {};
}
if (!Element.prototype.scrollTo) {
  Element.prototype.scrollTo = () => {};
}
// jsdom throws "Not implemented" for window.scrollTo; Kobalte's menu engages
// solid-prevent-scroll on open, which restores window scroll on close.
window.scrollTo = (() => {}) as typeof window.scrollTo;

// jsdom lacks matchMedia; the Composer probes `(pointer: coarse)` on mount.
if (!window.matchMedia) {
  window.matchMedia = ((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
    dispatchEvent: () => false,
  })) as typeof window.matchMedia;
}

// jsdom lacks ResizeObserver; the MessageScroller observes content growth.
if (!("ResizeObserver" in globalThis)) {
  class ResizeObserverStub {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
  (globalThis as unknown as { ResizeObserver: unknown }).ResizeObserver = ResizeObserverStub;
}

afterEach(() => cleanup());
