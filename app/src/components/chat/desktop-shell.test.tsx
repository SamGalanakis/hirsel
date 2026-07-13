import { fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";
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

  it("carries the desktop interrupt red on the rail header, with count parity to the phone shelf", async () => {
    await setup();
    const railBadge = document.querySelector('[data-slot="pings-rail-badge"]') as HTMLElement;
    const shelfBadge = document.querySelector('[data-slot="tray-shelf-badge"]') as HTMLElement;
    expect(railBadge).toBeTruthy();
    expect(shelfBadge).toBeTruthy();
    // Same COUNT (one open requires_response item) — both derive from
    // openUnreadCount, so the number holds parity by construction.
    expect(railBadge.textContent).toBe("1");
    expect(shelfBadge.textContent).toBe("1");
    // One-Escalation (v2.3): with the nav "Inbox" badge gone, the standing Pings
    // rail header is the ONE desktop home of the interrupt red — danger tone when
    // an open requires-response Ping exists (the seeded item is one). The phone
    // shelf keeps danger tone as the phone's one red.
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
  it("yields the rail to an open side chat and restores it from the NavRail Pings item", async () => {
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

    // The desktop restore affordance is the NavRail "Pings" item: selecting it
    // returns the region to `pings` — the rail comes back and the side chat
    // stays alive/resumable underneath (its sideChatRefs entry is untouched).
    await fireEvent.click(screen.getByLabelText("Pings"));
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
    store.openProcesses();

    const panel = await waitFor(
      () => document.querySelector('[data-slot="processes-panel"]') as HTMLElement,
    );
    expect(panel).toBeTruthy();
    // Phone base: a full-screen sheet. Desktop (v2.3 single-slot): an IN-FLOW
    // right-edge aside (relative, not an absolute overlay) at the shared Pings
    // width token — the inactive panes unmount, so nothing clips behind it.
    expect(panel.className).toContain("fixed");
    expect(panel.className).toContain("inset-0");
    expect(panel.className).toContain("rail:relative");
    expect(panel.className).toContain("rail:shrink-0");
    expect(panel.className).toContain("rail:w-[clamp(340px,38vw,440px)]");
    // Dialog semantics for the phone modal sheet (a11y §5).
    expect(panel.getAttribute("role")).toBe("dialog");
    // The chat is still live beside it (not unmounted/replaced) — the composer
    // remains in the tree.
    expect(screen.getByPlaceholderText("Message the Agent…")).toBeTruthy();
  });
});

describe("Desktop shell: the nav rail", () => {
  it("mounts a Primary nav with Pings / Processes / Settings items (no Chat/Inbox/Commands)", async () => {
    const { screen } = await setupApp();
    const nav = screen.getByRole("navigation", { name: "Primary" });
    expect(nav).toBeTruthy();
    // v2.3: the rail navigates the ONE right region. Chat is always the center
    // pane (no nav row); the standing rail IS the Pings surface (one "Pings"
    // item, no separate "Inbox"); the palette is ⌘K (no "Commands" row).
    expect(within(nav).getByLabelText("Pings")).toBeTruthy();
    expect(within(nav).getByRole("button", { name: /Processes/ })).toBeTruthy();
    expect(within(nav).getByLabelText("Settings")).toBeTruthy();
    expect(within(nav).queryByRole("button", { name: "Chat" })).toBeNull();
    expect(within(nav).queryByRole("button", { name: "Inbox" })).toBeNull();
    expect(within(nav).queryByRole("button", { name: /Commands/ })).toBeNull();
  });

  it("marks exactly the nav item that owns rightRegion as aria-current", async () => {
    const { screen, store } = await setupApp();
    const nav = screen.getByRole("navigation", { name: "Primary" });
    const pings = within(nav).getByLabelText("Pings");
    const processes = within(nav).getByRole("button", { name: /Processes/ });
    const settings = within(nav).getByLabelText("Settings");

    // Default resting state: Pings owns the region (no more "Chat active =
    // !processesOpen && !settingsOpen" fiction).
    expect(pings.getAttribute("aria-current")).toBe("page");
    expect(processes.getAttribute("aria-current")).toBeNull();
    expect(settings.getAttribute("aria-current")).toBeNull();

    store.openProcesses();
    await waitFor(() => expect(processes.getAttribute("aria-current")).toBe("page"));
    expect(pings.getAttribute("aria-current")).toBeNull();
    expect(settings.getAttribute("aria-current")).toBeNull();
  });

  it("carries a MUTED Pings count on the nav (the red lives on the rail header)", async () => {
    await setupApp();
    const navBadge = document.querySelector('[data-slot="nav-pings-badge"]') as HTMLElement;
    const shelfBadge = document.querySelector('[data-slot="tray-shelf-badge"]') as HTMLElement;
    expect(navBadge).toBeTruthy();
    expect(shelfBadge).toBeTruthy();
    // Count parity with the phone shelf — but the nav badge is ALWAYS muted (the
    // single red is the Pings rail header / phone shelf, never the nav).
    expect(navBadge.textContent).toBe("1");
    expect(shelfBadge.textContent).toBe("1");
    expect(navBadge.className).toContain("bg-muted-foreground");
    expect(navBadge.className).not.toContain("bg-status-danger");
    expect(shelfBadge.className).toContain("bg-status-danger");
  });

  it("opens the Processes inspector from the NavRail Processes item", async () => {
    const { screen } = await setupApp();
    const nav = screen.getByRole("navigation", { name: "Primary" });
    expect(document.querySelector('[data-slot="processes-panel"]')).toBeNull();
    await fireEvent.click(within(nav).getByRole("button", { name: /Processes/ }));
    await waitFor(() =>
      expect(document.querySelector('[data-slot="processes-panel"]')).toBeTruthy(),
    );
  });

  it("opens the Settings inspector from the NavRail gear", async () => {
    const { screen } = await setupApp();
    const nav = screen.getByRole("navigation", { name: "Primary" });
    expect(document.querySelector('[data-slot="settings-panel"]')).toBeNull();
    await fireEvent.click(within(nav).getByLabelText("Settings"));
    await waitFor(() =>
      expect(document.querySelector('[data-slot="settings-panel"]')).toBeTruthy(),
    );
  });

  it("also reaches Settings from the phone-header overflow (§4)", async () => {
    const { screen } = await setupApp();
    // On phone the header folds Model · Canvas · Processes · Settings behind one
    // ⋯ overflow so AgentStatus keeps its width. Settings is a menu item there.
    const user = userEvent.setup();
    expect(document.querySelector('[data-slot="settings-panel"]')).toBeNull();
    await user.click(screen.getByLabelText("More"));
    // The menu content is portaled to document.body (outside the container).
    await user.click(await within(document.body).findByRole("menuitem", { name: "Settings" }));
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
