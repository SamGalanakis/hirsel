import { fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { EventItem } from "../../protocol";

// Desktop-unified shell (desktop-unified pass). Desktop is now an EXPANDED view
// of mobile: Feed AND Chat stand side by side, decided and typed into without
// ever "navigating" — the old Queue/Chat destination rows and the standing Pings
// rail are gone. The named breakpoints are pure CSS, but the top-level shell now
// forks on a matchMedia flag (the phone and desktop trees are structurally
// different and only one mounts at a time), so these tests DRIVE matchMedia:
// `useRail()` reports the desktop width so `<App/>` mounts the DesktopShell.
// Live pixel behaviour at 390/1280/1680/2560 is verified in the browser.

/** matchMedia reporting the desktop (`rail`) width: only the `min-width: 1100px`
 * query matches; the coarse-pointer / phone max-width probes stay false (fine
 * pointer, non-modal inspectors), matching a real desktop client. */
function useRail() {
  window.matchMedia = ((query: string) => ({
    matches: /min-width:\s*1100px/.test(query),
    media: query,
    onchange: null,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
    dispatchEvent: () => false,
  })) as typeof window.matchMedia;
}

/** matchMedia reporting a phone: nothing matches (the base column). */
function usePhone() {
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

beforeEach(() => {
  vi.resetModules();
  useRail();
});
afterEach(() => usePhone());

function judgment(id: number, heading: string): EventItem {
  return {
    id,
    kind: "judgment",
    source: { kind: "agent", ref: "host" },
    name: `@j${id}`,
    description: heading,
    requires_response: true,
    quick_replies: [],
    status: "open",
    read: false,
    anchor: 0,
    ts: `2026-07-13T0${id}:00:00Z`,
    blocking: true,
    ui: [{ type: "heading", text: heading }],
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
    readEvent: vi.fn(),
    sendMessage: vi.fn(() => -1),
    sendEventAction: vi.fn(),
    retrySend: vi.fn(),
    cancelQueued: vi.fn(),
    cancelTurn: vi.fn(),
  };
}

/** Full-App mount at desktop width, ws/client fully stubbed. */
async function setupApp(events: EventItem[] = []) {
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
  // Seed events BEFORE render so the Feed's DEV mock-seed (fires only on an empty
  // set) never inflates a controlled count.
  for (const e of events) {
    store.dispatch({ type: "event_upsert", payload: { type: "event_upsert", event: e } });
  }
  const screen = render(() => <App />);
  return { store, screen, fakeClient };
}

describe("Desktop-unified shell: Feed and Chat stand together", () => {
  it("mounts BOTH the Feed column and the Chat composer at once — no destination switch", async () => {
    const { screen } = await setupApp([judgment(1, "Persist the canvas?")]);
    // The core of the unification: the Feed column and the always-mounted Chat
    // composer are BOTH present with zero navigation — desktop is an expanded
    // mobile, not a pair of destinations you toggle between.
    expect(document.querySelector('[data-slot="feed-column"]')).toBeTruthy();
    expect(screen.getByPlaceholderText("Message the Agent…")).toBeTruthy();
    // The Feed shows the seeded judgment's card content beside Chat.
    expect(screen.getByText("Persist the canvas?")).toBeTruthy();
    expect(document.querySelectorAll("main")).toHaveLength(1);
    const feed = document.querySelector('[data-slot="feed-column"]');
    expect(document.querySelector("main")?.contains(feed)).toBe(true);
  });

  it("carries the surface's ONE red on the Feed header", async () => {
    const { screen } = await setupApp([judgment(1, "Decide the schema")]);

    // The Feed header pill is the one red — a needs-you count in danger tone.
    const need = document.querySelector('[data-slot="feed-need"]') as HTMLElement;
    expect(need.textContent).toContain("need you");
    expect(need.className).toContain("status-danger");

    await waitFor(() => expect(screen.getByText("Decide the schema")).toBeTruthy());
  });
});

describe("Desktop-unified shell: the icon rail", () => {
  it("is a slim Primary nav with a Processes launcher — no Queue/Chat/Pings/Feed destination rows", async () => {
    const { screen } = await setupApp();
    const nav = screen.getByRole("navigation", { name: "Primary" });
    expect(nav).toBeTruthy();
    // Processes still docks the right region (the only launcher in the nav).
    expect(within(nav).getByRole("button", { name: /Processes/ })).toBeTruthy();
    // The destination rows are gone — Feed and Chat are always visible, so
    // "switching" to them is meaningless; Pings is retired (the Feed is the
    // needs-you surface now).
    expect(within(nav).queryByRole("button", { name: /Queue/ })).toBeNull();
    expect(within(nav).queryByRole("button", { name: /Feed/ })).toBeNull();
    expect(within(nav).queryByRole("button", { name: /^Chat/ })).toBeNull();
    expect(within(nav).queryByRole("button", { name: /Pings|Inbox/ })).toBeNull();
    // The brand folds to a monogram tile; the Settings gear + ⌘K live in the
    // footer strip, OUTSIDE the Primary nav.
    expect(screen.getByRole("img", { name: "hirsel" })).toBeTruthy();
    expect(within(nav).queryByLabelText("Settings")).toBeNull();
    expect(screen.getByLabelText("Settings")).toBeTruthy();
    expect(screen.getByRole("button", { name: "⌘K — Open command palette" })).toBeTruthy();
  });

  it("docks Processes / Settings into the shared right region, marking exactly that launcher aria-current", async () => {
    const { screen } = await setupApp();
    const nav = screen.getByRole("navigation", { name: "Primary" });
    const processes = within(nav).getByRole("button", { name: /Processes/ });
    const settings = screen.getByLabelText("Settings");

    // Resting: the right region is idle — nothing renders there, and no
    // launcher is current.
    expect(processes.getAttribute("aria-current")).toBeNull();
    expect(settings.getAttribute("aria-current")).toBeNull();
    expect(document.querySelector('[data-slot="processes-panel"]')).toBeNull();

    // Dock Processes from its launcher: the inspector appears and it is current.
    await fireEvent.click(processes);
    await waitFor(() =>
      expect(document.querySelector('[data-slot="processes-panel"]')).toBeTruthy(),
    );
    expect(processes.getAttribute("aria-current")).toBe("page");
    expect(settings.getAttribute("aria-current")).toBeNull();
    // Feed + Chat stay live beside the docked inspector.
    expect(document.querySelector('[data-slot="feed-column"]')).toBeTruthy();
    expect(screen.getByPlaceholderText("Message the Agent…")).toBeTruthy();

    // Dock Settings from the footer gear: it displaces Processes and is current.
    await fireEvent.click(settings);
    await waitFor(() =>
      expect(document.querySelector('[data-slot="settings-panel"]')).toBeTruthy(),
    );
    expect(settings.getAttribute("aria-current")).toBe("page");
    expect(processes.getAttribute("aria-current")).toBeNull();
  });
});

describe("Desktop-unified shell: right-region precedence", () => {
  it("yields the region to an open side chat and returns to the empty resting state on close", async () => {
    const { screen } = await setupApp();
    const store = await import("../../store/store");
    // No side chat surface at rest; Feed + Chat present.
    expect(document.querySelector('[data-slot="side-chat-sheet"]')).toBeNull();
    expect(document.querySelector('[data-slot="feed-column"]')).toBeTruthy();

    // Open a side chat: it takes the right region beside a still-live chat.
    store.openSideChat("side:1");
    await waitFor(() =>
      expect(document.querySelector('[data-slot="side-chat-sheet"]')).toBeTruthy(),
    );
    expect(screen.getByPlaceholderText("Message the Agent…")).toBeTruthy();

    // Closing returns the region to the idle state — which renders nothing — while the side chat
    // stays alive/resumable underneath.
    store.closeRightRegion();
    await waitFor(() =>
      expect(document.querySelector('[data-slot="side-chat-sheet"]')).toBeNull(),
    );
    expect(store.state.rightRegion).toBe("none");
    expect(store.state.activeSideChatSc).toBe("side:1");
    expect(document.querySelector('[data-slot="feed-column"]')).toBeTruthy();
  });
});

describe("Desktop-unified shell: the frame cap", () => {
  it("caps and centres the frame at the rail breakpoint instead of stretching", async () => {
    await setupApp();
    const frame = document.querySelector('[data-slot="app-frame"]') as HTMLElement;
    expect(frame).toBeTruthy();
    // Centred (mx-auto), phone-width by default, a ROW filled to a cap at `rail`.
    expect(frame.className).toContain("mx-auto");
    expect(frame.classList.contains("max-w-[560px]")).toBe(true);
    expect(frame.classList.contains("rail:flex-row")).toBe(true);
    expect(frame.classList.contains("rail:max-w-[1600px]")).toBe(true);
  });
});

describe("Settings is also reachable from the phone-header overflow (§4)", () => {
  it("opens the Settings inspector from the ⋯ overflow on phone", async () => {
    usePhone(); // this flow is the PHONE shell (the header overflow)
    await setupApp();
    const user = userEvent.setup();
    expect(document.querySelector('[data-slot="settings-panel"]')).toBeNull();
    const header = document.querySelector("header") as HTMLElement;
    await user.click(within(header).getByLabelText(/More actions/));
    await user.click(await within(document.body).findByRole("menuitem", { name: "Settings" }));
    await waitFor(() =>
      expect(document.querySelector('[data-slot="settings-panel"]')).toBeTruthy(),
    );
  });
});
