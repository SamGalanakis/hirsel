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
    // The task name is visible quiet context now, not screen-reader-only; the
    // generated question still visibly leads it (DESIGN §3).
    expect(identity.className).not.toContain("sr-only");
    expect(identity.className).toContain("text-muted-foreground");
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

  it("marks the focused chip, dims the rest, and offers a labelled exit", async () => {
    const { screen, store } = await setupApp([
      task(1, "@choose-direction", "Choose direction"),
      task(2, "@orchestration-copy", "Shape the orchestration copy"),
    ]);
    const nav = screen.getByRole("navigation", { name: "Tasks" });
    const first = within(nav).getByRole("button", { name: /choose direction, blocked on you/ });
    const second = within(nav).getByRole("button", { name: /orchestration copy, blocked on you/ });

    expect(first).not.toHaveAttribute("aria-current");
    await fireEvent.click(first);

    // The marker is a 2px accent rule on the edge the chip shares with the
    // field, never a tint, and the other chips step back.
    expect(first).toHaveAttribute("aria-current", "page");
    expect(first.className).toContain("border-b-2");
    expect(first.classList.contains("border-primary")).toBe(true);
    expect(first.classList.contains("bg-primary/[0.08]")).toBe(false);
    expect(second.classList.contains("opacity-55")).toBe(true);
    expect(first.classList.contains("opacity-55")).toBe(false);

    await fireEvent.click(within(nav).getByRole("button", { name: "Clear focus" }));
    expect(store.state.focusedTaskId).toBeNull();
    expect(first).not.toHaveAttribute("aria-current");
    expect(document.querySelector('[data-slot="ambient-field"]')).toBeInTheDocument();
  });

  it("lands `g t` on the focused chip rather than the top of the index", async () => {
    const { screen } = await setupApp([
      task(1, "@choose-direction", "Choose direction"),
      task(2, "@orchestration-copy", "Shape the orchestration copy"),
    ]);
    const nav = screen.getByRole("navigation", { name: "Tasks" });
    const second = within(nav).getByRole("button", { name: /orchestration copy, blocked on you/ });
    await fireEvent.click(second);
    screen.getByLabelText("Message Hirsel").blur();

    fireEvent.keyDown(window, { key: "g" });
    fireEvent.keyDown(window, { key: "t" });
    await waitFor(() => expect(second).toHaveFocus());
  });

  it("walks the Esc ladder: a live turn is stopped, an idle focused task is left", async () => {
    const { screen, store, fakeClient } = await setupApp([
      task(1, "@choose-direction", "Choose direction"),
    ]);
    await fireEvent.click(screen.getByRole("button", { name: /choose direction, blocked on you/ }));
    expect(store.state.focusedTaskId).toBe(1);

    // Rung 2: a running turn owns Esc — the composer stops it and focus stays.
    store.dispatch({ type: "agent_activity", payload: { state: "thinking", text: null } });
    const composer = screen.getByLabelText("Message Hirsel");
    composer.focus();
    fireEvent.keyDown(composer, { key: "Escape" });
    expect(fakeClient.cancelTurn).toHaveBeenCalledTimes(1);
    expect(store.state.focusedTaskId).toBe(1);
    expect(document.querySelector('[data-slot="task-field"]')).toBeInTheDocument();

    // Rung 3: idle again, the same key leaves the task for the ambient field —
    // and still works with the caret in the composer.
    store.dispatch({ type: "agent_activity", payload: { state: "idle", text: null } });
    fireEvent.keyDown(composer, { key: "Escape" });
    expect(fakeClient.cancelTurn).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(store.state.focusedTaskId).toBeNull());
    expect(document.querySelector('[data-slot="ambient-field"]')).toBeInTheDocument();
  });

  it("lets the generated instrument own the framing instead of repeating the description", async () => {
    const framed: EventItem = {
      ...task(1, "@rollout", "Roll out the migration"),
      ui: [{ type: "status", label: "Waiting on the migration window" }],
    };
    const { screen } = await setupApp([framed]);
    await fireEvent.click(screen.getByRole("button", { name: /rollout/ }));

    const field = document.querySelector('[data-slot="task-field"]') as HTMLElement;
    expect(field.textContent).toContain("Waiting on the migration window");
    // The description is the fallback title the instrument already stands in
    // for — printing both is the duplication this gate removes.
    expect(field.textContent).not.toContain("Roll out the migration");
  });

  it("still shows the description when the event carries no generated UI", async () => {
    const bare: EventItem = { ...task(1, "@rollout", "Roll out the migration"), ui: [] };
    const { screen } = await setupApp([bare]);
    await fireEvent.click(screen.getByRole("button", { name: /rollout/ }));

    expect(document.querySelector('[data-slot="task-field"]')?.textContent)
      .toContain("Roll out the migration");
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
    const phoneProcessesClose = within(processesPanel).getByText("Tasks").closest("button")!;
    fireEvent.click(phoneProcessesClose);
    await waitFor(() => expect(processesTrigger).toHaveFocus());

    store.openSettings();
    await waitFor(() => expect(store.state.rightRegion).toBe("settings"));
    const settingsPanel = document.querySelector('[data-slot="settings-panel"]') as HTMLElement;
    expect(within(settingsPanel).getByText("Tasks").closest("button")?.className)
      .toContain("min-h-11");
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

    const phoneClose = within(panel).getByText("Tasks").closest("button")!;
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
