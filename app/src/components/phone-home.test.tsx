import { render, waitFor } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";

// Craft wave: the phone Feed↔Chat swap is a directional cross-slide on a
// `state.home` change — Chat enters from the right, Feed enters from the left —
// reusing the sheet-slide vocabulary, gated so a fresh load does not slide.
beforeEach(() => {
  vi.resetModules();
});

describe("homeEnterClass — directional cross-slide", () => {
  it("slides Chat in from the right and Feed in from the left", async () => {
    const { homeEnterClass } = await import("./PhoneHome");
    expect(homeEnterClass("chat", true)).toContain("slide-in-from-right-4");
    expect(homeEnterClass("queue", true)).toContain("slide-in-from-left-4");
    // Both ride the shared sheet-slide vocabulary, motion-gated.
    expect(homeEnterClass("chat", true)).toContain("motion-safe:animate-in");
    expect(homeEnterClass("chat", true)).toContain("motion-safe:duration-200");
  });

  it("does not animate the first paint (a fresh load is not a navigation)", async () => {
    const { homeEnterClass } = await import("./PhoneHome");
    expect(homeEnterClass("chat", false)).toBe("");
    expect(homeEnterClass("queue", false)).toBe("");
  });
});

describe("PhoneHome — one surface mounted, sliding on home change", () => {
  it("shows Feed at rest and cross-slides to Chat on a home change", async () => {
    const store = await import("../store/store");
    const { PhoneHome } = await import("./PhoneHome");
    const screen = render(() => (
      <PhoneHome feed={() => <div data-testid="feed">FEED</div>} chat={() => <div data-testid="chat">CHAT</div>} />
    ));

    // At rest (home === "queue") only the Feed surface is mounted — never both.
    const feed = screen.container.querySelector('[data-surface="feed"]') as HTMLElement;
    expect(feed).not.toBeNull();
    expect(screen.container.querySelector('[data-surface="chat"]')).toBeNull();
    expect(screen.getByTestId("feed")).toBeTruthy();

    // Navigate to Chat: the Chat surface takes the column and enters from the right.
    store.goToChat();
    await waitFor(() => {
      const chat = screen.container.querySelector('[data-surface="chat"]') as HTMLElement;
      expect(chat).not.toBeNull();
      expect(chat.className).toContain("slide-in-from-right-4");
    });
    // Single-mount: the Feed surface has unmounted.
    expect(screen.container.querySelector('[data-surface="feed"]')).toBeNull();

    // Back to Feed: it enters from the left.
    store.goToQueue();
    await waitFor(() => {
      const back = screen.container.querySelector('[data-surface="feed"]') as HTMLElement;
      expect(back).not.toBeNull();
      expect(back.className).toContain("slide-in-from-left-4");
    });
  });
});
