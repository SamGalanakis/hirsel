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

afterEach(() => cleanup());
