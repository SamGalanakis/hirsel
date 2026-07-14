import { render } from "@solidjs/testing-library";
import { createRoot } from "solid-js";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { EventItem } from "../../protocol";
import { markArrival, resetArrivals } from "../../lib/event-entrance";
import { createCardEntrance, DecidedStrip } from "./EventCard";

// Craft wave: card entrance rhymes with the archive exit (genuine arrivals only,
// still under reduced motion) and the decided state crossfades in.

const realMatchMedia = window.matchMedia;

beforeEach(() => resetArrivals());
afterEach(() => {
  window.matchMedia = realMatchMedia;
});

function ev(overrides: Partial<EventItem> = {}): EventItem {
  return {
    id: 7,
    kind: "judgment",
    source: { kind: "agent", ref: "hirsel-host" },
    name: "@decide",
    description: "a judgment",
    ui: [{ type: "heading", text: "Which way?" }],
    requires_response: true,
    quick_replies: [],
    status: "open",
    read: false,
    anchor: 0,
    ts: "2026-07-13T09:00:00Z",
    ...overrides,
  };
}

/** Force `prefers-reduced-motion: reduce` (default jsdom matchMedia is false). */
function reduceMotion(reduced: boolean): void {
  window.matchMedia = ((query: string) => ({
    matches: reduced && query.includes("prefers-reduced-motion"),
    media: query,
    onchange: null,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
    dispatchEvent: () => false,
  })) as typeof window.matchMedia;
}

describe("createCardEntrance — entrance only for genuine arrivals", () => {
  it("animates in a freshly-arrived card (from-state + live transition)", () => {
    markArrival(7);
    createRoot((dispose) => {
      const { entering, atFrom } = createCardEntrance(ev());
      // At mount the card sits at the FROM offset with the transition live.
      expect(atFrom()).toBe(true);
      expect(entering()).toBe(true);
      dispose();
    });
  });

  it("does NOT animate a card that was never marked (initial hydration / re-render)", () => {
    createRoot((dispose) => {
      const { entering, atFrom } = createCardEntrance(ev());
      expect(atFrom()).toBe(false);
      expect(entering()).toBe(false);
      dispose();
    });
  });

  it("stays still under reduced motion even for a genuine arrival", () => {
    reduceMotion(true);
    markArrival(7);
    createRoot((dispose) => {
      const { entering, atFrom } = createCardEntrance(ev());
      // No from-state, no transition — the card is simply there.
      expect(atFrom()).toBe(false);
      expect(entering()).toBe(false);
      dispose();
    });
  });

  it("consumes the arrival flag so a second mount does not replay", () => {
    markArrival(7);
    createRoot((dispose) => {
      createCardEntrance(ev()); // consumes
      const second = createCardEntrance(ev());
      expect(second.atFrom()).toBe(false);
      expect(second.entering()).toBe(false);
      dispose();
    });
  });
});

describe("DecidedStrip — the decided state crossfades in", () => {
  it("fades the strip in and scales the success check once", () => {
    const { container } = render(() => <DecidedStrip ev={ev()} onUndo={() => {}} />);
    const strip = container.firstElementChild as HTMLElement;
    // The strip itself crossfades in (~150ms).
    expect(strip.className).toContain("motion-safe:animate-in");
    expect(strip.className).toContain("motion-safe:fade-in");
    // The green check scales in once (0.8→1).
    const check = container.querySelector(".rounded-full") as HTMLElement;
    expect(check.className).toContain("motion-safe:zoom-in-80");
  });
});
