import { fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { EventItem } from "../../protocol";

let railViewport = true;
let mediaQueries: Set<MediaQueryList> = new Set();

function mediaMatches(query: string): boolean {
  return /min-width:\s*1100px/.test(query)
    ? railViewport
    : /max-width:\s*1099\.98px/.test(query)
      ? !railViewport
      : false;
}

function matchMedia(matchesRail: boolean) {
  railViewport = matchesRail;
  mediaQueries = new Set();
  window.matchMedia = ((query: string) => {
    const listeners = new Set<(event: MediaQueryListEvent) => void>();
    const mql = {
      get matches() {
        return mediaMatches(query);
      },
      media: query,
      onchange: null,
      addEventListener: (_type: "change", listener: (event: MediaQueryListEvent) => void) => {
        listeners.add(listener);
      },
      removeEventListener: (_type: "change", listener: (event: MediaQueryListEvent) => void) => {
        listeners.delete(listener);
      },
      addListener: (listener: (event: MediaQueryListEvent) => void) => listeners.add(listener),
      removeListener: (listener: (event: MediaQueryListEvent) => void) => listeners.delete(listener),
      dispatchEvent: (event: Event) => {
        for (const listener of listeners) listener(event as MediaQueryListEvent);
        return true;
      },
    } as MediaQueryList;
    mediaQueries.add(mql);
    return mql;
  }) as typeof window.matchMedia;
}

function setRailViewport(matchesRail: boolean) {
  railViewport = matchesRail;
  for (const mql of mediaQueries) {
    mql.dispatchEvent(new Event("change"));
  }
  window.dispatchEvent(new Event("resize"));
}

beforeEach(() => {
  vi.resetModules();
  matchMedia(true);
});

function task(id: number, name: string, heading: string, anchor = 0): EventItem {
  return {
    id,
    kind: "judgment",
    source: { kind: "agent", ref: "host" },
    name,
    description: heading,
    requires_response: true,
    quick_replies: [],
    status: "open",
    read: false,
    anchor,
    ts: `2026-07-13T0${id}:00:00Z`,
    blocking: true,
    ui: [
      { type: "heading", text: heading },
      {
        type: "optionList",
        action: "choose",
        options: [
          { key: "A", label: "Move with the focused direction", recommended: true },
          { key: "B", label: "Keep exploring" },
        ],
      },
    ],
  };
}

function makeClient(store: typeof import("../../store/store")) {
  void store;
  return {
    resolvePing: vi.fn(),
    readEvent: vi.fn(),
    sendMessage: vi.fn(() => -1),
    sendEventAction: vi.fn(),
    retrySend: vi.fn(),
    cancelQueued: vi.fn(),
    cancelTurn: vi.fn(),
  };
}

async function setupApp(tasks: EventItem[]) {
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
  for (const event of tasks) {
    store.dispatch({ type: "event_upsert", payload: { type: "event_upsert", event } });
  }
  const screen = render(() => <App />);
  return { store, screen, fakeClient };
}

describe("Task Margins shell", () => {
  it("mounts one flat task world with a generated instrument and one Hirsel composer", async () => {
    const { screen } = await setupApp([task(1, "@choose-direction", "Choose the visual direction")]);

    expect(screen.getByRole("navigation", { name: "Tasks" })).toBeInTheDocument();
    expect(document.querySelector('[data-slot="ambient-field"]')).toBeInTheDocument();
    expect(screen.queryByText("Across everything")).toBeNull();
    expect(screen.queryByText("Global Hirsel")).toBeNull();
    expect(document.querySelector('[data-slot="task-field"]')).toBeNull();
    const composer = screen.getByLabelText("Message Hirsel");
    expect(composer).not.toHaveAttribute("placeholder");
    expect(document.querySelector('[data-slot="composer-shell"]')).toHaveAttribute(
      "data-focused",
      "false",
    );
    await fireEvent.click(screen.getByRole("button", { name: /choose direction, blocked on you/ }));
    expect(document.querySelector('[data-slot="task-field"]')).toBeInTheDocument();
    const identity = screen.getByRole("heading", { level: 2, name: "choose direction" });
    const question = screen.getByRole("heading", { level: 3, name: "Choose the visual direction" });
    expect(identity.className).toBe("sr-only");
    expect(question.className).toContain("clamp(1.75rem,3vw,2.25rem)");
    expect(document.querySelector('[data-slot="task-field"]')?.textContent).not.toContain("blocked on you");
    expect(screen.getByText("Move with the focused direction")).toBeInTheDocument();
    expect(document.querySelector('[data-slot="composer-shell"]')).toHaveAttribute(
      "data-focused",
      "true",
    );
    expect(document.querySelectorAll("main")).toHaveLength(1);
    expect(document.querySelector('[data-slot="feed-column"]')).toBeNull();
  });

  it("keeps task status accessible without turning ambient into a header mode", async () => {
    const { screen } = await setupApp([task(1, "@choose-direction", "Choose direction")]);
    expect(screen.queryByRole("button", { name: /All tasks/ })).toBeNull();
    expect(screen.getByRole("button", { name: /choose direction, blocked on you/ }))
      .toHaveAttribute("aria-pressed", "false");
  });

  it("opens another task and makes focus visible without a composer label", async () => {
    const { screen } = await setupApp([
      task(1, "@choose-direction", "Choose direction"),
      task(2, "@orchestration-copy", "Shape the orchestration copy"),
    ]);
    const nav = screen.getByRole("navigation", { name: "Tasks" });
    await fireEvent.click(within(nav).getByRole("button", { name: /orchestration copy/ }));

    expect(screen.getByRole("heading", { name: "orchestration copy" })).toBeInTheDocument();
    expect(within(nav).getByRole("button", { name: /orchestration copy/ }))
      .toHaveAttribute("aria-pressed", "true");
    expect(document.querySelector('[data-slot="composer-shell"]')).toHaveAttribute(
      "data-focused",
      "true",
    );
    expect(document.querySelector('[data-slot="task-scope"]')).toBeNull();
  });

  it("moves task focus and selection together with roving arrow keys", async () => {
    const { screen } = await setupApp([
      task(1, "@choose-direction", "Choose direction"),
      task(2, "@orchestration-copy", "Shape the orchestration copy"),
    ]);
    const nav = screen.getByRole("navigation", { name: "Tasks" });
    const first = within(nav).getByRole("button", { name: /choose direction, blocked on you/ });
    const second = within(nav).getByRole("button", { name: /orchestration copy, blocked on you/ });

    first.focus();
    fireEvent.keyDown(first, { key: "ArrowDown" });
    await waitFor(() => {
      expect(second).toHaveFocus();
      expect(second).toHaveAttribute("aria-pressed", "true");
    });
    expect(screen.getByRole("heading", { name: "orchestration copy" })).toBeInTheDocument();
  });

  it("returns to ambient by selecting the focused task again", async () => {
    const { screen } = await setupApp([task(1, "@choose-direction", "Choose direction")]);
    const taskButton = screen.getByRole("button", { name: /choose direction, blocked on you/ });
    await fireEvent.click(taskButton);
    expect(taskButton).toHaveAttribute("aria-pressed", "true");
    await fireEvent.click(taskButton);

    expect(taskButton).toHaveAttribute("aria-pressed", "false");
    expect(document.querySelector('[data-slot="ambient-field"]')).toBeInTheDocument();
    expect(document.querySelector('[data-slot="task-field"]')).toBeNull();
    expect(document.querySelector('[data-slot="composer-shell"]')).toHaveAttribute(
      "data-focused",
      "false",
    );
    expect(document.querySelector('[data-slot="task-scope"]')).toBeNull();
    expect(screen.queryByText("Ambient")).toBeNull();
    expect(screen.queryByText("Global Hirsel")).toBeNull();
  });

  it("sends focused messages through the global transcript with task mention and anchor", async () => {
    const { screen, fakeClient } = await setupApp([
      task(7, "@choose-direction", "Choose direction", 42),
    ]);
    await fireEvent.click(screen.getByRole("button", { name: /choose direction, blocked on you/ }));
    const user = userEvent.setup();
    await user.type(screen.getByLabelText("Message Hirsel"), "Take option A");
    await user.click(screen.getByLabelText("Send"));

    await waitFor(() => expect(fakeClient.sendMessage).toHaveBeenCalledTimes(1));
    expect(fakeClient.sendMessage).toHaveBeenCalledWith(
      "Take option A",
      42,
      expect.objectContaining({ mentions: [7] }),
    );
  });

  it("sends ambient messages without a task anchor", async () => {
    const { screen, fakeClient } = await setupApp([
      task(7, "@choose-direction", "Choose direction", 42),
    ]);
    const user = userEvent.setup();
    await user.type(screen.getByLabelText("Message Hirsel"), "Synthesize everything");
    await user.click(screen.getByLabelText("Send"));

    await waitFor(() => expect(fakeClient.sendMessage).toHaveBeenCalledTimes(1));
    expect(fakeClient.sendMessage).toHaveBeenCalledWith(
      "Synthesize everything",
      null,
      expect.objectContaining({ mentions: [] }),
    );
  });

  it("summons utilities over the same task world", async () => {
    const { store, screen } = await setupApp([task(1, "@choose-direction", "Choose direction")]);
    await fireEvent.click(screen.getByRole("button", { name: /choose direction, blocked on you/ }));

    store.openProcesses();
    await waitFor(() => expect(document.querySelector('[data-slot="processes-panel"]')).toBeInTheDocument());
    expect(screen.getByLabelText("Message Hirsel")).toBeInTheDocument();

    store.openSettings();
    await waitFor(() => expect(document.querySelector('[data-slot="settings-panel"]')).toBeInTheDocument());
    expect(document.querySelector('[data-slot="processes-panel"]')).toBeNull();

    expect(screen.getByRole("heading", { name: "choose direction" })).toBeInTheDocument();
  });

  it("restores phone Processes focus to its dedicated trigger and keeps Settings close touch-sized", async () => {
    matchMedia(false);
    const { screen, store } = await setupApp([task(1, "@choose-direction", "Choose direction")]);
    const user = userEvent.setup();
    const processesTrigger = screen.getByRole("button", { name: "Processes" });

    await user.click(processesTrigger);
    await waitFor(() => expect(store.state.rightRegion).toBe("processes"));
    const processesPanel = document.querySelector('[data-slot="processes-panel"]') as HTMLElement;
    // ONE header, one exit: a phone sheet carries exactly one "Close Processes"
    // control and no back-to-Tasks chevron — hirsel has no navigation stack.
    expect(within(processesPanel).queryByText("Tasks")).toBeNull();
    const processesClose = within(processesPanel).getByLabelText("Close Processes");
    fireEvent.click(processesClose);
    await waitFor(() => expect(processesTrigger).toHaveFocus());

    store.openSettings();
    await waitFor(() => expect(store.state.rightRegion).toBe("settings"));
    const settingsPanel = document.querySelector('[data-slot="settings-panel"]') as HTMLElement;
    expect(within(settingsPanel).queryByText("Tasks")).toBeNull();
    expect(within(settingsPanel).getByLabelText("Close Settings").className)
      .toContain("[@media(pointer:coarse)]:size-11");
  });

  it("restores a desktop-opened Processes utility to its phone trigger after a viewport change", async () => {
    const { screen, store } = await setupApp([task(1, "@choose-direction", "Choose direction")]);
    const composer = screen.getByLabelText("Message Hirsel");
    const processesTrigger = screen.getByRole("button", { name: "Processes" });
    composer.focus();

    // A keyboard or command can summon the desktop inspector without using the
    // overflow trigger. Resizing while it is open must change the eventual
    // restoration target to the standing Processes control.
    store.openProcesses();
    const panel = await waitFor(() => {
      const node = document.querySelector('[data-slot="processes-panel"]') as HTMLElement;
      expect(node).toHaveAttribute("role", "complementary");
      return node;
    });

    setRailViewport(false);
    await waitFor(() => expect(panel).toHaveAttribute("role", "dialog"));
    await waitFor(() => expect(panel.contains(document.activeElement)).toBe(true));

    const phoneClose = within(panel).getByLabelText("Close Processes");
    phoneClose.focus();
    fireEvent.keyDown(phoneClose, { key: "Tab", shiftKey: true });
    expect(panel.contains(document.activeElement)).toBe(true);

    // The open sheet owns shortcuts; `/` must not jump behind it to Composer.
    fireEvent.keyDown(window, { key: "/" });
    expect(composer).not.toHaveFocus();

    fireEvent.click(phoneClose);
    await waitFor(() => expect(processesTrigger).toHaveFocus());
  });

  it("uses the same responsive shell on phone", async () => {
    matchMedia(false);
    const { screen } = await setupApp([task(1, "@choose-direction", "Choose direction")]);
    const shell = document.querySelector('[data-slot="task-shell"]') as HTMLElement;
    const overflow = document.querySelector('[data-slot="phone-overflow-trigger"]') as HTMLElement;

    expect(shell.className).toContain("max-w-[1600px]");
    expect(overflow.className).toContain("shrink-0");
    expect(overflow.className).toContain("[@media(pointer:coarse)]:size-11");
    expect(screen.queryByRole("button", { name: /All tasks/ })).toBeNull();
    expect(screen.getByRole("navigation", { name: "Tasks" })).toBeInTheDocument();
    expect(screen.getByLabelText("Message Hirsel")).not.toHaveAttribute("placeholder");
    expect(document.querySelector('[data-slot="ambient-field"]')).toBeInTheDocument();
    expect(document.querySelector('[data-slot="phone-nav"]')).toBeNull();
  });
});
