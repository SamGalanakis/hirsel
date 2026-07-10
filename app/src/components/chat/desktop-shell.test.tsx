import { fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Ping } from "../../protocol";

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

function inboxItem(overrides: Partial<Ping> = {}): Ping {
  return {
    id: 1,
    name: "deploy-approval",
    description: "Approve the production deployment",
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
    openSideChat: vi.fn((pingId: number) => {
      const sc = `side:${++scCounter}`;
      store.dispatch({ type: "side_chat_open", sc, pingId, messages: [] });
    }),
    sendSideMessage: vi.fn(() => -1),
    cancelSideTurn: vi.fn(),
    concludeSideChat: vi.fn(),
    confirmConclusion: vi.fn(),
    discardSideChat: vi.fn(),
    resolvePing: vi.fn(),
    readPing: vi.fn(),
    sendMessage: vi.fn(() => -1),
    retrySend: vi.fn(),
    cancelQueued: vi.fn(),
    cancelTurn: vi.fn(),
  };
}

async function setup(itemOverrides: Partial<Ping> = {}) {
  const store = await import("../../store/store");
  const fakeClient = makeClient(store);
  vi.doMock("../../ws/client", () => ({ getClient: () => fakeClient }));

  const { ChatView } = await import("./ChatView");
  store.dispatch({ type: "connection_status", status: "connected" });
  store.dispatch({
    type: "ping_upsert",
    payload: { type: "ping_upsert", ping: inboxItem(itemOverrides) },
  });
  const screen = render(() => <ChatView />);
  return { store, screen, fakeClient };
}

/** Full-App mount (the header — with its restore-Pings affordance — lives in
 * App, not ChatView). ws/client is fully stubbed so no socket opens. */
async function setupApp(itemOverrides: Partial<Ping> = {}) {
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
    type: "ping_upsert",
    payload: { type: "ping_upsert", ping: inboxItem(itemOverrides) },
  });
  const screen = render(() => <App />);
  return { store, screen, fakeClient };
}

describe("Desktop shell: the standing Pings rail", () => {
  it("stands a rail that reuses PingsView and is CSS-gated to the rail breakpoint", async () => {
    const { screen } = await setup();
    const rail = screen.getByRole("complementary", { name: "Pings" });
    // CSS-gated: hidden below `rail`, shown as a flex column at/above it — so it
    // renders at ≥1100 and never competes with the phone shelf below.
    expect(rail.className).toContain("hidden");
    expect(rail.className).toContain("rail:flex");
    // Reuses PingsView verbatim — the seeded item card is in the rail body.
    expect(within(rail).getByText("Approve the deploy to prod?")).toBeTruthy();
  });

  it("carries a muted rail badge with count parity to the phone shelf", async () => {
    await setup();
    const railBadge = document.querySelector('[data-slot="pings-rail-badge"]') as HTMLElement;
    const shelfBadge = document.querySelector('[data-slot="tray-shelf-badge"]') as HTMLElement;
    expect(railBadge).toBeTruthy();
    expect(shelfBadge).toBeTruthy();
    // Same COUNT (one open requires_response item) — both derive from
    // openUnreadCount, so the number holds parity by construction.
    expect(railBadge.textContent).toBe("1");
    expect(shelfBadge.textContent).toBe("1");
    // One-Escalation: the rail header count is NEVER the interrupt red — on
    // desktop the single sanctioned red is the nav Inbox badge (asserted in the
    // nav-rail suite). The phone shelf keeps danger tone as the phone's one red.
    expect(railBadge.className).toContain("bg-muted-foreground");
    expect(railBadge.className).not.toContain("bg-status-danger");
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
  it("yields the rail to an open side chat and restores it from the NavRail Inbox item", async () => {
    const { screen } = await setupApp();
    // Default: rail present, no side chat surface.
    expect(screen.queryByRole("complementary", { name: "Pings" })).toBeTruthy();
    expect(document.querySelector('[data-slot="side-chat-sheet"]')).toBeNull();

    // Open a side chat from the rail: it takes the right region; the rail
    // yields (unmounts). Only the rail mounts PingsView here (tray collapsed),
    // so the Discuss control is unambiguous.
    await fireEvent.click(
      within(screen.getByRole("complementary", { name: "Pings" })).getByRole("button", {
        name: /Discuss/,
      }),
    );
    await waitFor(() => expect(document.querySelector('[data-slot="side-chat-sheet"]')).toBeTruthy());
    expect(screen.queryByRole("complementary", { name: "Pings" })).toBeNull();

    // The desktop restore affordance moved to the NavRail Inbox item: clicking
    // it brings the rail back (leaving the side chat alive/resumable — it only
    // clears the active sheet, not the sideChatRefs entry).
    await fireEvent.click(screen.getByLabelText("Inbox"));
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

describe("Desktop shell: the nav rail", () => {
  it("mounts a Primary nav with Chat / Inbox / Processes / Settings items", async () => {
    const { screen } = await setupApp();
    const nav = screen.getByRole("navigation", { name: "Primary" });
    expect(nav).toBeTruthy();
    // Each primary destination is a labeled control in the rail.
    expect(within(nav).getByRole("button", { name: "Chat" })).toBeTruthy();
    expect(within(nav).getByLabelText("Inbox")).toBeTruthy();
    expect(within(nav).getByRole("button", { name: /Processes/ })).toBeTruthy();
    expect(within(nav).getByLabelText("Settings")).toBeTruthy();
  });

  it("carries an Inbox badge with parity to the phone shelf badge", async () => {
    await setupApp();
    const navBadge = document.querySelector('[data-slot="nav-inbox-badge"]') as HTMLElement;
    const shelfBadge = document.querySelector('[data-slot="tray-shelf-badge"]') as HTMLElement;
    expect(navBadge).toBeTruthy();
    expect(shelfBadge).toBeTruthy();
    // Same count and same danger tone (one open requires_response item) — both
    // derive from openUnreadCount / hasOpenRequiresResponse, so parity holds by
    // construction; this pins it.
    expect(navBadge.textContent).toBe("1");
    expect(shelfBadge.textContent).toBe("1");
    expect(navBadge.className).toContain("bg-status-danger");
    expect(shelfBadge.className).toContain("bg-status-danger");
  });

  it("opens the Processes inspector from the NavRail Processes item", async () => {
    const { screen } = await setupApp();
    // Scope to the rail: the phone header's ProcessesButton also matches
    // /Processes/, so pick the NavRail item specifically.
    const nav = screen.getByRole("navigation", { name: "Primary" });
    expect(document.querySelector('[data-slot="processes-panel"]')).toBeNull();
    await fireEvent.click(within(nav).getByRole("button", { name: /Processes/ }));
    await waitFor(() =>
      expect(document.querySelector('[data-slot="processes-panel"]')).toBeTruthy(),
    );
  });

  it("opens the Settings inspector from the NavRail gear", async () => {
    const { screen } = await setupApp();
    expect(document.querySelector('[data-slot="settings-panel"]')).toBeNull();
    await fireEvent.click(screen.getByLabelText("Settings"));
    await waitFor(() =>
      expect(document.querySelector('[data-slot="settings-panel"]')).toBeTruthy(),
    );
  });
});

describe("Desktop shell: the frame cap", () => {
  it("caps and centres the frame at the rail breakpoint instead of stretching", async () => {
    await setupApp();
    const frame = document.querySelector('[data-slot="app-frame"]') as HTMLElement;
    expect(frame).toBeTruthy();
    // Centred (mx-auto), phone-width by default, and a ROW filled to a cap at
    // `rail` — the 3-pane shell (nav ∣ chat ∣ context) uses the width via real
    // structure, a bounded fill never stretched to glass. The 2560 void is fixed
    // by this cap, which holds at every width ≥ rail.
    expect(frame.className).toContain("mx-auto");
    expect(frame.classList.contains("max-w-[560px]")).toBe(true);
    expect(frame.classList.contains("rail:flex-row")).toBe(true);
    expect(frame.classList.contains("rail:max-w-[1600px]")).toBe(true);
  });
});
