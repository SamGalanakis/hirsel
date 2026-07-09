import { fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { InboxItem } from "../../protocol";

// Desktop-shell gates (desktop-shell pass). The named breakpoints are pure CSS,
// so jsdom (which loads no stylesheet) can't compute which surface is visible at
// a given width — as the rest of this suite does for responsive behaviour, these
// assert on the structural facts instead: which components mount, which
// precedence gate is active, and that each surface carries the `rail:`/`split:`
// variant that scopes it to the desktop width. Live pixel behaviour at
// 390/1280/1680/2560 is verified in the browser separately.
beforeEach(() => {
  vi.resetModules();
});

function inboxItem(overrides: Partial<InboxItem> = {}): InboxItem {
  return {
    id: 1,
    content: "Approve the deploy to prod?",
    anchor: 5,
    requires_response: true,
    read: false,
    quick_replies: [],
    status: "open",
    ts: "2026-07-08T00:00:00Z",
    ...overrides,
  };
}

function makeClient(store: typeof import("../../store/store")) {
  let scCounter = 0;
  return {
    openSideChat: vi.fn((itemId: number) => {
      const sc = `side:${++scCounter}`;
      store.dispatch({ type: "side_chat_open", sc, itemId, messages: [] });
    }),
    sendSideMessage: vi.fn(() => -1),
    cancelSideTurn: vi.fn(),
    concludeSideChat: vi.fn(),
    confirmConclusion: vi.fn(),
    discardSideChat: vi.fn(),
    archiveItem: vi.fn(),
    readItem: vi.fn(),
    sendMessage: vi.fn(() => -1),
    retrySend: vi.fn(),
    cancelQueued: vi.fn(),
    cancelTurn: vi.fn(),
  };
}

async function setup(itemOverrides: Partial<InboxItem> = {}) {
  const store = await import("../../store/store");
  const fakeClient = makeClient(store);
  vi.doMock("../../ws/client", () => ({ getClient: () => fakeClient }));

  const { ChatView } = await import("./ChatView");
  store.dispatch({ type: "connection_status", status: "connected" });
  store.dispatch({
    type: "inbox_upsert",
    payload: { type: "inbox_upsert", item: inboxItem(itemOverrides) },
  });
  const screen = render(() => <ChatView />);
  return { store, screen, fakeClient };
}

/** Full-App mount (the header — with its restore-Pings affordance — lives in
 * App, not ChatView). ws/client is fully stubbed so no socket opens. */
async function setupApp(itemOverrides: Partial<InboxItem> = {}) {
  const store = await import("../../store/store");
  const fakeClient = makeClient(store);
  vi.doMock("../../ws/client", () => ({
    getStoredToken: () => "tok",
    setStoredToken: vi.fn(),
    startClient: () => ({ close: vi.fn() }),
    getClient: () => fakeClient,
  }));

  const { default: App } = await import("../../App");
  store.dispatch({ type: "connection_status", status: "connected" });
  store.dispatch({
    type: "inbox_upsert",
    payload: { type: "inbox_upsert", item: inboxItem(itemOverrides) },
  });
  const screen = render(() => <App />);
  return { store, screen, fakeClient };
}

describe("Desktop shell: the standing Pings rail", () => {
  it("stands a rail that reuses InboxView and is CSS-gated to the rail breakpoint", async () => {
    const { screen } = await setup();
    const rail = screen.getByRole("complementary", { name: "Pings" });
    // CSS-gated: hidden below `rail`, shown as a flex column at/above it — so it
    // renders at ≥1100 and never competes with the phone shelf below.
    expect(rail.className).toContain("hidden");
    expect(rail.className).toContain("rail:flex");
    // Reuses InboxView verbatim — the seeded item card is in the rail body.
    expect(within(rail).getByText("Approve the deploy to prod?")).toBeTruthy();
  });

  it("carries a badge with parity to the phone shelf", async () => {
    await setup();
    const railBadge = document.querySelector('[data-slot="pings-rail-badge"]') as HTMLElement;
    const shelfBadge = document.querySelector('[data-slot="tray-shelf-badge"]') as HTMLElement;
    expect(railBadge).toBeTruthy();
    expect(shelfBadge).toBeTruthy();
    // Same count and same danger tone (one open requires_response item) — both
    // derive from openUnreadCount / hasOpenRequiresResponse, so parity holds by
    // construction; this pins it.
    expect(railBadge.textContent).toBe("1");
    expect(shelfBadge.textContent).toBe("1");
    expect(railBadge.className).toContain("bg-status-danger");
    expect(shelfBadge.className).toContain("bg-status-danger");
  });

  it("keeps the phone shelf mounted as the sub-rail Pings surface", async () => {
    const { screen } = await setup();
    // The shelf is unconditional (additive rail above it) — the phone pattern is
    // untouched; nothing about it is `rail:`-gated away.
    expect(screen.getByLabelText("Open Pings")).toBeTruthy();
  });
});

describe("Desktop shell: right-region precedence", () => {
  it("yields the rail to an open side chat and restores it from the header", async () => {
    const { screen } = await setupApp();
    // Default: rail present, no side chat surface.
    expect(screen.queryByRole("complementary", { name: "Pings" })).toBeTruthy();
    expect(document.querySelector('[data-slot="side-chat-sheet"]')).toBeNull();

    // Open a side chat from the rail: it takes the right region; the rail
    // yields (unmounts). Only the rail mounts InboxView here (tray collapsed),
    // so the Discuss control is unambiguous.
    await fireEvent.click(
      within(screen.getByRole("complementary", { name: "Pings" })).getByRole("button", {
        name: /Discuss/,
      }),
    );
    await waitFor(() => expect(document.querySelector('[data-slot="side-chat-sheet"]')).toBeTruthy());
    expect(screen.queryByRole("complementary", { name: "Pings" })).toBeNull();

    // The header restore affordance appears and brings the rail back (leaving
    // the side chat alive/resumable).
    const restore = screen.getByLabelText("Show Pings");
    expect(restore.className).toContain("rail:flex");
    await fireEvent.click(restore);
    await waitFor(() => expect(screen.queryByRole("complementary", { name: "Pings" })).toBeTruthy());
    expect(document.querySelector('[data-slot="side-chat-sheet"]')).toBeNull();
  });

  it("docks Processes to the right region without unmounting the chat", async () => {
    const { store, screen } = await setup();
    store.dispatch({
      type: "process_upsert",
      payload: {
        type: "process_upsert",
        process: {
          id: "p1",
          kind: "subagent",
          label: "Review the auth refactor",
          agent: "code-reviewer",
          model: "gpt-5.5",
          state: "running",
          started_ts: "2026-07-08T00:00:00Z",
          last_event_ts: "2026-07-08T00:00:00Z",
          summary: "reading changed files…",
        },
      },
    });
    store.setProcessesOpen(true);

    const panel = await waitFor(
      () => document.querySelector('[data-slot="processes-panel"]') as HTMLElement,
    );
    expect(panel).toBeTruthy();
    // Phone base: a full-screen sheet. Desktop: docks to the right edge of the
    // frame (absolute, right-anchored, bounded width) — never the full bleed
    // that flung the status pill across the void, and never over the chat.
    expect(panel.className).toContain("fixed");
    expect(panel.className).toContain("inset-0");
    expect(panel.className).toContain("rail:absolute");
    expect(panel.className).toContain("rail:left-auto");
    expect(panel.className).toContain("rail:w-[420px]");
    // The chat is still live behind it (not unmounted/replaced) — the composer
    // remains in the tree.
    expect(screen.getByPlaceholderText("Message the Agent…")).toBeTruthy();
  });
});

describe("Desktop shell: the frame cap", () => {
  it("caps and centres the frame at the rail breakpoint instead of stretching", async () => {
    await setupApp();
    const frame = document.querySelector('[data-slot="app-frame"]') as HTMLElement;
    expect(frame).toBeTruthy();
    // Centred (mx-auto), phone-width by default, filled to a cap at `rail` — a
    // bounded fill, never a stretch-to-glass. The 2560 void is fixed by this
    // cap, which holds at every width ≥ rail.
    expect(frame.className).toContain("mx-auto");
    expect(frame.classList.contains("max-w-[560px]")).toBe(true);
    expect(frame.classList.contains("rail:max-w-[1360px]")).toBe(true);
  });
});
