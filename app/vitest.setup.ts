import "@testing-library/jest-dom/vitest";
import { cleanup } from "@solidjs/testing-library";
import { afterEach } from "vitest";
import { startPlugins } from "./src/plugins/loader";

// Shell tests simulate socket auth, which kicks off the once-per-load plugin
// roster fetch — in jsdom that fetch fails seconds later and its console.warn
// can land after the test file finished, racing worker teardown. Latch the
// loader with an empty roster so no test ever does network. loader.test.ts
// resetModules()s, so its fresh instances are unaffected.
startPlugins({ list: async () => [], modules: {} });

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
