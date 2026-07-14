import { fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { EventItem } from "../protocol";

// Each test runs against a pristine copy of the store singleton: resetModules +
// dynamic import means the freshly-imported component and the store `dispatch`
// we drive it with share the same new module instance.
beforeEach(() => {
  vi.resetModules();
});

/** An open judgment — the queue's "needs you" hero — used to prove the Feed
 * button's muted cross-surface count. */
function openJudgment(id: number): EventItem {
  return {
    id,
    kind: "judgment",
    source: { kind: "agent", ref: null },
    name: "change-approval",
    description: "Approve the change?",
    ui: { type: "text", text: "Approve?" },
    requires_response: true,
    quick_replies: [],
    status: "open",
    read: false,
    anchor: id,
    ts: "2026-07-13T00:00:00Z",
  };
}

async function setup() {
  const store = await import("../store/store");
  const { PhoneNavBar } = await import("./PhoneNavBar");
  const screen = render(() => <PhoneNavBar />);
  const bar = screen.container.querySelector('[data-slot="phone-nav-bar"]') as HTMLElement;
  return { store, screen, bar };
}

describe("PhoneNavBar — the phone bottom two-way nav (Feed / Chat)", () => {
  it("renders exactly two ghost icon nav buttons (Feed / Chat), phone-gated, with no composer field", async () => {
    const { bar } = await setup();
    // Exactly two destinations — Feed and Chat — each a real BUTTON, not input.
    const feed = within(bar).getByRole("button", { name: "Feed" });
    const chat = within(bar).getByRole("button", { name: "Chat" });
    expect(feed.tagName).toBe("BUTTON");
    expect(chat.tagName).toBe("BUTTON");
    expect(within(bar).getAllByRole("button")).toHaveLength(2);
    // Icon-led with a glyph each; the bar is navigation, never a message field.
    expect(feed.querySelector("svg")).toBeTruthy();
    expect(chat.querySelector("svg")).toBeTruthy();
    expect(bar.querySelector("input, textarea")).toBeNull();
    // The dead composer pill is gone — no "Message…" placeholder banner.
    expect(bar.textContent).not.toContain("Message");
    // Phone only — CSS-gated off at `rail` (desktop has the NavRail).
    expect(bar.className).toContain("rail:hidden");
  });

  it("is visible on BOTH homes and marks the active surface with a truthful aria-current", async () => {
    const { store, bar } = await setup();
    // Queue home: the bar is present and Feed is current, Chat is not.
    expect(store.state.home).toBe("queue");
    expect(within(bar).getByRole("button", { name: "Feed" })).toHaveAttribute("aria-current", "page");
    expect(within(bar).getByRole("button", { name: "Chat" })).not.toHaveAttribute("aria-current");

    // Drill into Chat: the SAME bar stays visible and now Chat is current — so
    // there is always a way back. Exactly one destination is current at a time.
    store.goToChat();
    await waitFor(() =>
      expect(within(bar).getByRole("button", { name: "Chat" })).toHaveAttribute(
        "aria-current",
        "page",
      ),
    );
    expect(within(bar).getByRole("button", { name: "Feed" })).not.toHaveAttribute("aria-current");
  });

  it("navigates BOTH ways: Chat drills in from the queue, Feed goes back from Chat", async () => {
    const { store, bar } = await setup();
    // Forward: queue → chat.
    fireEvent.click(within(bar).getByRole("button", { name: "Chat" }));
    expect(store.state.home).toBe("chat");
    // Back: chat → queue (the whole point — the Owner can always get back).
    fireEvent.click(within(bar).getByRole("button", { name: "Feed" }));
    expect(store.state.home).toBe("queue");
  });

  it("is pure navigation — tapping Chat does NOT trigger the composer autofocus intent", async () => {
    const { store, bar } = await setup();
    fireEvent.click(within(bar).getByRole("button", { name: "Chat" }));
    // The bar never duplicates input: no keyboard-up composer autofocus (that
    // intent stays for other callers, just not the nav bar).
    expect(store.state.composerAutofocus).toBe(false);
  });

  it("carries the queue's needs-you count on Feed only while away in Chat, in the muted style (never red)", async () => {
    const { store, bar } = await setup();
    store.dispatch({
      type: "event_upsert",
      payload: { type: "event_upsert", event: openJudgment(1) },
    });
    store.dispatch({
      type: "event_upsert",
      payload: { type: "event_upsert", event: openJudgment(2) },
    });

    // On the Feed home itself the count is HIDDEN — it would just duplicate the
    // queue's own "N need you" pager pill.
    expect(bar.querySelector('[data-slot="phone-nav-feed-badge"]')).toBeNull();

    // Away in Chat, the muted count appears as quiet cross-surface awareness.
    store.goToChat();
    const badge = await waitFor(() => {
      const el = bar.querySelector('[data-slot="phone-nav-feed-badge"]');
      expect(el).toBeTruthy();
      return el as HTMLElement;
    });
    expect(badge.textContent).toBe("2");
    // One-Escalation: nothing in the bar is red — the count is the muted style.
    expect(bar.querySelector('[class*="status-danger"], [class*="destructive"]')).toBeNull();
    expect(badge.className).toContain("bg-muted-foreground");
  });

  it("stays in normal flow so the scroller/chat height already accounts for it", async () => {
    // In-flow flex sibling below the surface — NOT fixed/absolute — so the
    // scroller's own `clientHeight` shrinks by the bar's height with no `svh`
    // math. Assert the layout contract (jsdom computes no real layout).
    const { bar } = await setup();
    expect(bar.className).toContain("flex-shrink-0");
    expect(bar.className).not.toContain("absolute");
    expect(bar.className).not.toContain("fixed");
  });
});

describe("PhoneOverflowMenu — no duplicated bottom-bar destinations", () => {
  it("has no Chat row (it lives on the bottom nav bar), but keeps Model / Processes / Settings", async () => {
    const { PhoneOverflowMenu } = await import("./PhoneOverflowMenu");
    const screen = render(() => <PhoneOverflowMenu />);
    const user = userEvent.setup();
    await user.click(screen.getByLabelText("More actions"));
    const menu = within(document.body);
    // The remaining secondary controls are still here…
    expect(await menu.findByRole("menuitem", { name: "Processes" })).toBeTruthy();
    expect(menu.getByRole("menuitem", { name: "Settings" })).toBeTruthy();
    // …but Chat is NOT — "bottom bar stuff shouldn't be at the top as well."
    expect(menu.queryByRole("menuitem", { name: "Chat" })).toBeNull();
  });
});
