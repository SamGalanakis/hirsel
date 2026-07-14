import { fireEvent, render, within } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ProcessInfo } from "../protocol";

beforeEach(() => {
  vi.resetModules();
});

function runningProcess(id: string): ProcessInfo {
  return {
    id,
    kind: "subagent",
    label: `proc-${id}`,
    agent: null,
    model: null,
    state: "running",
    started_ts: "2026-07-13T00:00:00Z",
    last_event_ts: "2026-07-13T00:00:00Z",
    summary: null,
  };
}

async function setup() {
  const store = await import("../store/store");
  const { PhoneChatBar } = await import("./PhoneChatBar");
  const screen = render(() => <PhoneChatBar />);
  const bar = screen.container.querySelector('[data-slot="phone-chat-bar"]') as HTMLElement;
  return { store, screen, bar };
}

describe("PhoneChatBar — the phone queue-home standing chat bar", () => {
  it("renders a compact dormant composer pill (bubble glyph + muted placeholder) and is phone-gated", async () => {
    const { screen, bar } = await setup();
    // The pill is a BUTTON (a door), not a live text field — tapping opens Chat.
    const pill = within(bar).getByRole("button", { name: "Message the agent" });
    expect(pill.tagName).toBe("BUTTON");
    // Compact, icon-led: a short muted placeholder (not the loud full "Message
    // the Agent…" banner) and a leading bubble glyph.
    expect(within(pill).getByText("Message…")).toBeTruthy();
    expect(pill.querySelector("svg")).toBeTruthy();
    // Phone only — CSS-gated off at `rail` (desktop has the NavRail Chat row).
    expect(bar.className).toContain("rail:hidden");
    // No text-labeled destinations / tab bar: the pill is the only labeled
    // control at rest, and it carries no visible "Chat" tab text.
    expect(bar.textContent).not.toContain("Chat");
    void screen;
  });

  it("taps into Chat and records the composer-focus intent (keyboard up)", async () => {
    const { store, bar } = await setup();
    expect(store.state.home).toBe("queue");
    fireEvent.click(within(bar).getByRole("button", { name: "Message the agent" }));
    // Drills into the chat shell AND asks it to land with the composer focused.
    expect(store.state.home).toBe("chat");
    expect(store.state.composerAutofocus).toBe(true);
  });

  it("shows the side icons only when they carry live state, and stays quiet otherwise", async () => {
    const { store, screen, bar } = await setup();
    // At rest: just the pill — no standing empty chrome, no badge slots.
    expect(within(bar).queryByRole("button", { name: /Processes/ })).toBeNull();
    expect(within(bar).queryByRole("button", { name: "Open canvas" })).toBeNull();

    // Work starts running → the Processes affordance appears with a count in its
    // accessible name (the visual tint chip is decorative).
    store.dispatch({
      type: "process_upsert",
      payload: { type: "process_upsert", process: runningProcess("p1") },
    });
    const procBtn = await within(bar).findByRole("button", { name: "Processes, 1 running" });
    expect(procBtn).toBeTruthy();

    // A canvas view arrives → the Canvas reopen affordance appears too (at most
    // two side icons).
    store.dispatch({
      type: "view_upsert",
      payload: {
        type: "view_upsert",
        instance_id: "c1",
        placement: "canvas",
        spec: { type: "text", text: "hi" },
      },
    });
    expect(await within(bar).findByRole("button", { name: "Open canvas" })).toBeTruthy();
    // Still no red anywhere in the bar (One-Escalation): the count is status-active.
    expect(bar.querySelector('[class*="status-danger"]')).toBeNull();
    void screen;
  });

  it("stays in normal flow so the scroller height already accounts for it", async () => {
    // The bar is an in-flow flex sibling below the scroller column — NOT fixed/
    // absolute — so the scroller's own `clientHeight` (its snap `pageHeight()`)
    // shrinks by the bar's height with no `svh` math. Assert the layout contract
    // that makes that true (jsdom computes no real layout to measure directly).
    const { bar } = await setup();
    expect(bar.className).toContain("flex-shrink-0");
    expect(bar.className).not.toContain("absolute");
    expect(bar.className).not.toContain("fixed");
  });
});
