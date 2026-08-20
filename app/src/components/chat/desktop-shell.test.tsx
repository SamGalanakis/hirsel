import { fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { EventKind } from "../../protocol";
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
    kind: EventKind.Judgment,
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
    await fireEvent.click(within(nav).getByRole("button", { name: /^orchestration copy,/ }));

    expect(screen.getByRole("heading", { name: "orchestration copy" })).toBeInTheDocument();
    expect(within(nav).getByRole("button", { name: /^orchestration copy,/ }))
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
    // The capsule fills its frame and the frame IS the reading measure plus a
    // gutter each side, so the capsule needs no width of its own — it used to
    // carry a `rail:max-w-measure` that could only ever disagree with the
    // column above it.
    expect(ambient.capsule).toContain("w-full");
    expect(ambient.capsule.filter((c) => /max-w-/.test(c))).toEqual([]);
    expect(ambient.outer).toContain("max-w-frame");
    expect(ambient.outer).toContain("px-gutter");
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

  it("marks the focused chip, dims the rest, and exits when the open chip is activated again", async () => {
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

    // The chip's accessible name promises this exit, and the chip itself keeps
    // the promise — there is no separate × any more.
    expect(first).toHaveAccessibleName(/focused; activate to clear focus/);
    await fireEvent.click(first);
    expect(store.state.focusedTaskId).toBeNull();
    expect(first).not.toHaveAttribute("aria-current");
    expect(document.querySelector('[data-slot="ambient-field"]')).toBeInTheDocument();
  });

  it("gives every chip a ⋯ menu whose Delete archives the task", async () => {
    const user = userEvent.setup();
    const { screen, store, fakeClient } = await setupApp([
      task(1, "@choose-direction", "Choose direction"),
      task(2, "@orchestration-copy", "Shape the orchestration copy"),
    ]);
    const nav = screen.getByRole("navigation", { name: "Tasks" });

    // Every chip carries it, not only the focused one — and no × survives.
    expect(within(nav).queryByRole("button", { name: "Clear focus" })).toBeNull();
    expect(within(nav).getByRole("button", { name: "Actions for choose direction" }))
      .toBeInTheDocument();
    const trigger = within(nav).getByRole("button", {
      name: "Actions for orchestration copy",
    });

    await user.click(trigger);
    const item = await within(document.body).findByRole("menuitem", { name: "Delete" });
    // The menu owns the keyboard while it is open, so bare keys stay suppressed.
    const { anyOverlayOpen } = await import("../../lib/focus");
    expect(anyOverlayOpen()).toBe(true);

    await user.click(item);
    expect(fakeClient.sendEventAction).toHaveBeenCalledWith(2, "archive", {});
    // Optimistic: the chip leaves the rail at once, and the toast offers Undo.
    await waitFor(() =>
      expect(within(nav).queryByRole("button", { name: /orchestration copy/ })).toBeNull()
    );
    expect(store.state.eventOverrides[2]).toMatchObject({ archived: true });
    expect(screen.getByText("Archived")).toBeInTheDocument();
    await waitFor(() => expect(anyOverlayOpen()).toBe(false));
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
    await fireEvent.click(screen.getByRole("button", { name: /^rollout,/ }));

    const field = document.querySelector('[data-slot="task-field"]') as HTMLElement;
    expect(field.textContent).toContain("Waiting on the migration window");
    // The description is the fallback title the instrument already stands in
    // for — printing both is the duplication this gate removes.
    expect(field.textContent).not.toContain("Roll out the migration");
  });

  it("still shows the description when the event carries no generated UI", async () => {
    const bare: EventItem = { ...task(1, "@rollout", "Roll out the migration"), ui: [] };
    const { screen } = await setupApp([bare]);
    await fireEvent.click(screen.getByRole("button", { name: /^rollout,/ }));

    expect(document.querySelector('[data-slot="task-field"]')?.textContent)
      .toContain("Roll out the migration");
  });

  it("sends focused messages through the global transcript with task mention and anchor", async () => {
    const { screen, fakeClient } = await setupApp([
      task(7, "@choose-direction", "Choose direction", 42),
    ]);
    await fireEvent.click(screen.getByRole("button", { name: /choose direction, blocked on you/ }));
    const user = userEvent.setup();
    // Enter IS the send on a fine pointer; the capsule carries no Send button
    // there any more (composer redesign).
    await user.type(screen.getByLabelText("Message Hirsel"), "Take option A{Enter}");
    expect(screen.queryByLabelText("Send")).toBeNull();

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
    await user.type(screen.getByLabelText("Message Hirsel"), "Synthesize everything{Enter}");
    expect(screen.queryByLabelText("Send")).toBeNull();

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

describe("Right region: Settings takes the screen, Processes stays beside the work", () => {
  it("presents Settings as one full-viewport modal at every width", async () => {
    const { store, screen } = await setupApp([task(1, "@choose-direction", "Choose direction")]);
    await fireEvent.click(screen.getByRole("button", { name: /choose direction, blocked on you/ }));
    store.openSettings();
    const panel = await waitFor(() => {
      const node = document.querySelector('[data-slot="settings-panel"]') as HTMLElement;
      expect(node).toBeInTheDocument();
      return node;
    });

    // Full-viewport at the rail width too: every docked-inspector override is
    // gone, so there is ONE Settings presentation rather than two.
    expect(panel.className).toContain("fixed");
    expect(panel.className).toContain("inset-0");
    expect(panel.className).not.toMatch(/(^|\s)rail:/);
    expect(panel.className).not.toMatch(/w-\[clamp\(/);
    // ...and it is an honest modal there, where it used to be a non-modal aside.
    expect(panel).toHaveAttribute("role", "dialog");
    expect(panel).toHaveAttribute("aria-modal", "true");

    // A generous reading measure, not a 340px rail — and the SAME one the task
    // world holds, header row included, so nothing invents a bespoke width.
    const column = panel.querySelector('[data-slot="settings-column"]') as HTMLElement;
    expect(column.className).toContain("max-w-frame");
    expect(column.className).toContain("mx-auto");
    expect(document.getElementById("settings-pane-title")?.parentElement?.className)
      .toContain("max-w-frame");

    // Tab is trapped now that it is modal at this width.
    const close = within(panel).getByLabelText("Close Settings");
    close.focus();
    fireEvent.keyDown(close, { key: "Tab", shiftKey: true });
    expect(panel.contains(document.activeElement)).toBe(true);
  });

  it("keeps Processes a docked inspector beside the field", async () => {
    const { store } = await setupApp([task(1, "@choose-direction", "Choose direction")]);
    store.openProcesses();
    const panel = await waitFor(() => {
      const node = document.querySelector('[data-slot="processes-panel"]') as HTMLElement;
      expect(node).toBeInTheDocument();
      return node;
    });
    // Processes is a glance kept beside the work: still in-flow at `rail`,
    // still non-modal there, still the shared utility width.
    expect(panel).toHaveAttribute("role", "complementary");
    expect(panel.className).toContain("rail:relative");
    expect(panel.className).toContain("rail:w-[clamp(340px,38vw,440px)]");
  });

  it("lets Escape dismiss the full-screen Settings without clearing task focus", async () => {
    const { store, screen } = await setupApp([task(1, "@choose-direction", "Choose direction")]);
    await fireEvent.click(screen.getByRole("button", { name: /choose direction, blocked on you/ }));
    expect(store.state.focusedTaskId).toBe(1);

    store.openSettings();
    await waitFor(() =>
      expect(document.querySelector('[data-slot="settings-panel"]')).toBeInTheDocument()
    );
    // The trap consumes Esc entirely, so the keymap's Esc ladder never advances
    // to its last rung on the same keystroke: the utility closes, the task stays.
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(store.state.rightRegion).toBe("none"));
    expect(store.state.focusedTaskId).toBe(1);
    expect(document.querySelector('[data-slot="task-field"]')).toBeInTheDocument();
  });
});

describe("The floating ⋯ yields to a docked pane", () => {
  it("anchors the affordance strip to the home field, not to the viewport", async () => {
    const { store } = await setupApp([task(1, "@choose-direction", "Choose direction")]);
    const affordances = document.querySelector('[data-slot="home-affordances"]') as HTMLElement;
    const home = document.querySelector('[data-slot="home-region"]') as HTMLElement;

    // The strip is positioned against the home field (index + task field), not
    // against the shell: it used to hang off the shell's top-right corner, i.e.
    // off the VIEWPORT, which is precisely why a docked pane slid underneath it.
    expect(affordances.parentElement).toBe(home);
    expect(affordances.className).toContain("absolute");
    expect(home.className).toContain("relative");
    // ...and it keeps a full gutter of air inside that corner.
    expect(affordances.className).toContain("px-gutter");

    store.openProcesses();
    const panel = await waitFor(() => {
      const node = document.querySelector('[data-slot="processes-panel"]') as HTMLElement;
      expect(node).toBeInTheDocument();
      return node;
    });

    // The docked pane is OUTSIDE the box the ⋯ is measured against, and is the
    // home field's own row sibling — so it takes its width out of that box and
    // the ⋯ (with the pane's close × inside it) cannot overlap at any width or
    // pointer type. Generic: this holds for whatever owns the region.
    expect(home.contains(panel)).toBe(false);
    expect(panel.parentElement).toBe(home.parentElement);
    expect(home.compareDocumentPosition(panel) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(home.className).toContain("flex-1");
    expect(home.className).toContain("min-w-0");
    expect(panel.className).toContain("rail:shrink-0");
    // Nothing may re-pin the strip to the shell or the viewport.
    expect(affordances.className).not.toContain("fixed");
    const shell = document.querySelector('[data-slot="task-shell"]') as HTMLElement;
    expect(affordances.parentElement).not.toBe(shell);
  });

  it("puts the Canvas rail under the same rule", async () => {
    const { store } = await setupApp([task(1, "@choose-direction", "Choose direction")]);
    store.dispatch({
      type: "view_upsert",
      payload: {
        type: "view_upsert",
        instance_id: "v1",
        placement: "canvas",
        spec: { type: "text", text: "drawn" },
      },
    } as never);
    store.showCanvas();
    const rail = await waitFor(() => {
      const node = document.querySelector('[data-slot="canvas-rail"]') as HTMLElement;
      expect(node).toBeInTheDocument();
      return node;
    });
    const home = document.querySelector('[data-slot="home-region"]') as HTMLElement;
    expect(home.contains(rail)).toBe(false);
    expect(rail.parentElement).toBe(home.parentElement);
  });
});

describe("Opens focused by default", () => {
  const moving = (id: number, name: string): EventItem => ({
    ...task(id, name, name),
    kind: EventKind.Summary,
    blocking: false,
    read: true,
    requires_response: false,
  });

  it("never gives a housekeeping info event a chip, focus, or the red count", async () => {
    const info = (id: number, name: string): EventItem => ({
      ...task(id, name, name),
      kind: EventKind.Info,
      blocking: false,
      requires_response: true,
      status: "open",
    });
    const { screen, store } = await setupApp([info(1, "@session-rotated")], {
      keepDefaultFocus: true,
    });

    const nav = screen.getByRole("navigation", { name: "Tasks" });
    expect(within(nav).queryByRole("button", { name: /session rotated/ })).toBeNull();
    // Nothing in the field wants the Owner, so the field stays ambient…
    expect(store.state.focusedTaskId).toBeNull();
    expect(document.querySelector('[data-slot="ambient-field"]')).toBeInTheDocument();
    // …and the tab badge stays silent even though the event is open and claims
    // to require a response.
    expect(document.title).toBe("hirsel");
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
      expect(screen.getByRole("button", { name: /^urgent call,/ })).toBeInTheDocument()
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
