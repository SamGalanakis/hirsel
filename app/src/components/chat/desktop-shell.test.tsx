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

async function setupApp(tasks: EventItem[], options: { keepDefaultFocus?: boolean } = {}) {
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
  // Load now opens focused on the most-needing task (see "Opens focused by
  // default" below). These gates exercise selection FROM the ambient field,
  // which is one Esc away, so they start from the cleared state.
  if (!options.keepDefaultFocus) store.clearTaskFocus();
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

  it("keeps the composer geometrically identical across the focus swap", async () => {
    const { screen } = await setupApp([task(1, "@choose-direction", "Choose direction")]);
    // Everything that can place or size the capsule. The composer is the one
    // element on screen through every state, so the swap may change its tone
    // and nothing else: it used to narrow from the frame width to the reading
    // measure on focus, which slid it and its send target sideways on every
    // toggle (DESIGN §4, §5).
    const geometric = /^(rail:|split:)?(w-|min-w-|max-w-|mx-|ml-|mr-|px-|pl-|pr-|py-|basis-|flex-|justify-|self-)/;
    const shape = () => {
      const shell = document.querySelector('[data-slot="composer-shell"]') as HTMLElement;
      return {
        outer: (shell.parentElement as HTMLElement).className.split(/\s+/).filter((c) => geometric.test(c)).sort(),
        capsule: shell.className.split(/\s+/).filter((c) => geometric.test(c)).sort(),
      };
    };

    const ambient = shape();
    expect(ambient.capsule).toContain("w-full");
    expect(ambient.capsule).toContain("rail:max-w-measure");
    await fireEvent.click(screen.getByRole("button", { name: /choose direction, blocked on you/ }));
    expect(document.querySelector('[data-slot="composer-shell"]')).toHaveAttribute("data-focused", "true");
    expect(shape()).toEqual(ambient);
  });

  it("swaps the two fields with one shared motion and one shared inset", async () => {
    const { screen } = await setupApp([task(1, "@choose-direction", "Choose direction")]);
    // Focus and ambient are one surface changing subject: the same enter
    // animation and the same vertical inset, so neither the motion nor the
    // resting position of the content betrays which field is on screen.
    const swap = /^motion-safe:|^p[xy]-|^rail:py-/;
    const marks = (slot: string) =>
      (document.querySelector(`[data-slot="${slot}"]`) as HTMLElement)
        .className.split(/\s+/).filter((c) => swap.test(c)).sort();

    const ambient = marks("ambient-field");
    expect(ambient).toContain("motion-safe:animate-in");
    expect(ambient).toContain("motion-safe:duration-200");
    await fireEvent.click(screen.getByRole("button", { name: /choose direction, blocked on you/ }));
    expect(marks("task-field")).toEqual(ambient);
  });

  it("stacks the task card and the conversation in ONE column, never two", async () => {
    const { screen, store } = await setupApp([task(1, "@choose-direction", "Choose direction", 5)]);
    store.dispatch({
      type: "msg",
      payload: {
        type: "msg",
        message: { id: 5, author: "agent", body: "Which direction?", ref: null, ts: "2026-07-13T02:00:00Z", attachments: [] },
      },
    });
    await fireEvent.click(screen.getByRole("button", { name: /choose direction, blocked on you/ }));

    const column = document.querySelector('[data-slot="task-column"]') as HTMLElement;
    const card = document.querySelector('[data-slot="task-card"]') as HTMLElement;
    const conversation = document.querySelector('[data-slot="task-conversation"]') as HTMLElement;
    // Same container, card first: the conversation is no longer a margin beside
    // the instrument, it is the flow underneath it (DESIGN §4).
    expect(column.contains(card)).toBe(true);
    expect(column.contains(conversation)).toBe(true);
    expect(card.compareDocumentPosition(conversation) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(within(conversation).getByText("Which direction?")).toBeInTheDocument();

    // One width for the whole column — the same measure the composer holds.
    expect(column.className).toContain("max-w-measure");
    // Nothing may reintroduce a second track, at any width or container size.
    const field = document.querySelector('[data-slot="task-field"]') as HTMLElement;
    for (const node of [field, column, card, conversation]) {
      expect(node.className).not.toMatch(/grid-cols-\[minmax\(0,1fr\)_/);
      expect(node.className).not.toContain("@[46rem]");
    }
    // Wide content still scrolls inside its own box on both halves.
    expect(card.className).toContain("grid-cols-[minmax(0,1fr)]");
    expect(conversation.className).toContain("grid-cols-[minmax(0,1fr)]");
  });

  it("pins the task card and caps its height so the conversation is never pushed off", async () => {
    const { screen } = await setupApp([task(1, "@choose-direction", "Choose direction")]);
    await fireEvent.click(screen.getByRole("button", { name: /choose direction, blocked on you/ }));
    const card = document.querySelector('[data-slot="task-card"]') as HTMLElement;

    // Sticky at the top of the scroll container: task context stays legible
    // while the history below it is read.
    expect(card.className).toContain("sticky");
    expect(card.className).toContain("top-0");
    // A tall instrument stops at the cap and scrolls inside itself rather than
    // shoving the conversation out of the field.
    expect(card.className).toContain("max-h-[40dvh]");
    expect(card.className).toContain("overflow-y-auto");
    // ...and it never traps the wheel: the pinned card is what most gestures
    // land on, so scroll chaining stays at its default.
    expect(card.className).not.toContain("overscroll-contain");
    // It rides on the canvas with one hairline, not on a decorative surface.
    expect(card.className).toContain("bg-background");
    expect(card.className).toContain("border-b");
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

  it("restores phone Processes focus to the floating overflow and keeps Settings close touch-sized", async () => {
    matchMedia(false);
    const { store } = await setupApp([task(1, "@choose-direction", "Choose direction")]);
    const user = userEvent.setup({ pointerEventsCheck: 0 });
    // The home header bar is gone: Processes is reached from the one floating
    // ⋯, and that trigger is what a dismissed sheet restores to.
    const processesTrigger = document.querySelector(
      '[data-slot="phone-overflow-trigger"]',
    ) as HTMLElement;

    await user.click(processesTrigger);
    await user.click(await within(document.body).findByRole("menuitem", { name: /Processes/ }));
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
    const processesTrigger = document.querySelector(
      '[data-slot="phone-overflow-trigger"]',
    ) as HTMLElement;
    composer.focus();

    // A keyboard or command can summon the desktop inspector without opening
    // the overflow. Resizing while it is open must change the eventual
    // restoration target to the floating overflow trigger.
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

describe("Home header collapse", () => {
  it("leaves one floating overflow instead of a bar, and no standing status", async () => {
    const { screen } = await setupApp([task(1, "@choose-direction", "Choose direction")]);
    const shell = document.querySelector('[data-slot="task-shell"]') as HTMLElement;

    // No bar: no brandmark/wordmark, no standing Processes button, and the
    // shell's own top anchor is its content.
    expect(shell.querySelector("header")).toBeNull();
    expect(screen.queryByRole("heading", { name: "hirsel" })).toBeNull();
    expect(document.querySelector('[data-slot="processes-trigger"]')).toBeNull();

    const affordances = document.querySelector('[data-slot="home-affordances"]') as HTMLElement;
    expect(affordances.className).toContain("absolute");
    expect(affordances.contains(
      document.querySelector('[data-slot="phone-overflow-trigger"]'),
    )).toBe(true);

    // Connected is silence: nothing VISIBLE reports it (the only "connected"
    // in there is the sr-only live region asserted in the next gate).
    expect(affordances.querySelector('[data-slot="badge"]')).toBeNull();
  });

  it("shows connection state only while it is abnormal, and keeps it announced", async () => {
    const { store } = await setupApp([task(1, "@choose-direction", "Choose direction")]);
    const affordances = document.querySelector('[data-slot="home-affordances"]') as HTMLElement;

    const live = within(affordances).getByRole("status");
    expect(live).toHaveAttribute("aria-live", "polite");
    expect(live.textContent).toBe("connected");
    // Nothing is drawn for a healthy socket; the live region is sr-only.
    expect(live.className).toContain("sr-only");
    expect(affordances.querySelector('[data-slot="badge"]')).toBeNull();

    store.dispatch({ type: "connection_status", status: "reconnecting" });
    await waitFor(() => expect(affordances.textContent).toContain("reconnecting"));
    expect(live.textContent).toBe("reconnecting…");

    store.dispatch({ type: "connection_status", status: "connected" });
    // Recovery is announced by the same never-unmounted live region, even
    // though the visible pill has already left.
    await waitFor(() => expect(live.textContent).toBe("connected"));
    expect(affordances.querySelector('[data-slot="badge"]')).toBeNull();
  });

  it("reaches Processes and Settings from that one overflow", async () => {
    const { store } = await setupApp([task(1, "@choose-direction", "Choose direction")]);
    // The dismissed sheet's overlay leaves `pointer-events: none` on <body> for
    // longer than the panel itself lives; this gate is about what the menu
    // reaches, not that teardown timing.
    const user = userEvent.setup({ pointerEventsCheck: 0 });
    store.dispatch({
      type: "process_upsert",
      payload: {
        type: "process_upsert",
        process: {
          id: "p1",
          kind: "subagent",
          label: "Review the auth refactor",
          agent: null,
          model: null,
          state: "running",
          started_ts: "2026-07-13T01:00:00Z",
          last_event_ts: "2026-07-13T01:00:00Z",
          summary: null,
        },
      },
    });

    await user.click(document.querySelector('[data-slot="phone-overflow-trigger"]') as HTMLElement);
    const processes = await within(document.body).findByRole("menuitem", { name: /Processes/ });
    // Live work still announces its count without a bar to carry it.
    expect(processes.textContent).toContain("1");
    await user.click(processes);
    await waitFor(() => expect(store.state.rightRegion).toBe("processes"));

    // Back to the field, then the same menu reaches Settings.
    store.closeRightRegion();
    await waitFor(() =>
      expect(document.querySelector('[data-slot="processes-panel"]')).toBeNull()
    );
    await user.click(document.querySelector('[data-slot="phone-overflow-trigger"]') as HTMLElement);
    await user.click(await within(document.body).findByRole("menuitem", { name: "Settings" }));
    await waitFor(() => expect(store.state.rightRegion).toBe("settings"));
  });
});

describe("Opens focused by default", () => {
  const moving = (id: number, name: string): EventItem => ({
    ...task(id, name, name),
    kind: "summary",
    blocking: false,
    read: true,
    requires_response: false,
  });

  it("lands on the most-needing task rather than the ambient field", async () => {
    const { screen, store } = await setupApp(
      [moving(1, "@rollout"), task(2, "@choose-direction", "Choose direction")],
      { keepDefaultFocus: true },
    );

    expect(store.state.focusedTaskId).toBe(2);
    expect(document.querySelector('[data-slot="task-field"]')).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /choose direction, blocked on you/ }))
      .toHaveAttribute("aria-pressed", "true");
  });

  it("stays ambient after Esc, and a later task never steals focus back", async () => {
    const { screen, store } = await setupApp(
      [task(1, "@choose-direction", "Choose direction")],
      { keepDefaultFocus: true },
    );
    expect(store.state.focusedTaskId).toBe(1);

    const composer = screen.getByLabelText("Message Hirsel");
    composer.focus();
    fireEvent.keyDown(composer, { key: "Escape" });
    await waitFor(() => expect(store.state.focusedTaskId).toBeNull());

    store.dispatch({
      type: "event_upsert",
      payload: { type: "event_upsert", event: task(2, "@urgent-call", "Urgent call") },
    });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /urgent call/ })).toBeInTheDocument()
    );
    expect(store.state.focusedTaskId).toBeNull();
    expect(document.querySelector('[data-slot="ambient-field"]')).toBeInTheDocument();

    // A reconnect re-enters "connected"; auto-focus is once per LOAD.
    store.dispatch({ type: "connection_status", status: "reconnecting" });
    store.dispatch({ type: "connection_status", status: "connected" });
    expect(store.state.focusedTaskId).toBeNull();
  });

  it("rests ambient when there is no open task", async () => {
    const { store } = await setupApp([], { keepDefaultFocus: true });
    expect(store.state.focusedTaskId).toBeNull();
    expect(document.querySelector('[data-slot="ambient-field"]')).toBeInTheDocument();
  });
});
