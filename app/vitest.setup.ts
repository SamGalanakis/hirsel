import "@testing-library/jest-dom/vitest";
import { cleanup, configure } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { afterEach } from "vitest";
import { startPlugins } from "./src/plugins/loader";

// Solid 2 batches event writes; DOM assertions observe the committed event.
configure({ eventWrapper: callback => flush(callback) });

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

// jsdom has the dialog element but not its browser-managed open/close methods.
// Real focus traversal and iframe navigation are covered by browser smoke tests.
if (!HTMLDialogElement.prototype.showModal) {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.show = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
}

afterEach(() => cleanup());
