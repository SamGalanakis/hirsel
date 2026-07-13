import { fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { EventItem } from "../../protocol";

beforeEach(() => {
  vi.resetModules();
});

function judgment(id: number, heading: string, blocking = false): EventItem {
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
    blocking,
    ui: [
      { type: "heading", text: heading },
      {
        type: "optionList",
        action: "choose",
        options: [
          { key: "A", recommended: true, label: `Pick A for ${id}` },
          { key: "B", label: `Pick B for ${id}` },
        ],
      },
    ],
  };
}

async function setup(events: EventItem[]) {
  const store = await import("../../store/store");
  const toast = await import("../../lib/toast");
  const sent: { eventId: number; action: string; data: unknown }[] = [];
  vi.doMock("../../ws/client", () => ({
    getClient: () => ({
      sendEventAction: (eventId: number, action: string, data: unknown) =>
        sent.push({ eventId, action, data }),
    }),
  }));
  for (const e of events) {
    store.dispatch({ type: "event_upsert", payload: { type: "event_upsert", event: e } });
  }
  const { EventScroller } = await import("./EventScroller");
  const screen = render(() => <EventScroller />);
  const reader = screen.container.querySelector('[data-slot="event-scroller"]') as HTMLElement;
  const list = screen.container.querySelector('[data-slot="queue-list"]') as HTMLElement;
  return { store, toast, screen, sent, reader, list };
}

describe("EventScroller — the vertical event queue home", () => {
  it("shows the ONE red needs-you count, leads with the blocking judgment, and stands the two-column index", async () => {
    const { screen, reader, list } = await setup([
      judgment(1, "Second judgment"),
      judgment(2, "Blocking judgment", true),
    ]);
    // The pager carries the surface's single red — two open judgments need you.
    const need = screen.container.querySelector('[data-slot="pager-need"]') as HTMLElement;
    expect(need.textContent).toBe("2 need you");
    expect(need.className).toContain("status-danger");
    // The reader card column renders the cards; the blocking one leads.
    expect(within(reader).getByText("Blocking judgment")).toBeTruthy();
    expect(within(reader).getByText("Second judgment")).toBeTruthy();
    // §1: the standing queue index stands beside it, CSS-gated to `rail` (hidden
    // below, an in-flow column at/above it), and indexes the same events.
    expect(list).toBeTruthy();
    expect(list.className).toContain("hidden");
    expect(list.className).toContain("rail:flex");
    expect(within(list).getAllByText("Blocking judgment").length).toBe(1);
  });

  it("posts an event_action, flips the card to decided, and drops the needs-you count", async () => {
    const { store, screen, sent, reader } = await setup([judgment(5, "Only judgment")]);
    // Tap the recommended option.
    fireEvent.click(screen.getByRole("button", { name: /Pick A for 5/ }));
    // Optimistic decide + the exact wire action.
    expect(store.state.eventDecideOverrides).toContain(5);
    expect(sent).toEqual([{ eventId: 5, action: "choose", data: { choice: "A", label: "Pick A for 5" } }]);
    // The card flips to the decided strip and the count clears.
    await waitFor(() => expect(within(reader).getByText("decided")).toBeTruthy());
    const need = screen.container.querySelector('[data-slot="pager-need"]') as HTMLElement;
    expect(need.textContent).toBe("all clear");
  });

  it("keeps exactly one Undo: the decided strip on-screen, no toast (§6)", async () => {
    const { screen, toast, reader } = await setup([judgment(5, "Only judgment")]);
    fireEvent.click(screen.getByRole("button", { name: /Pick A for 5/ }));
    await waitFor(() => expect(within(reader).getByText("decided")).toBeTruthy());
    // The on-card strip carries the Undo; no toast is raised while it is visible,
    // so the two Undos never stack.
    expect(within(reader).getByRole("button", { name: "Undo" })).toBeTruthy();
    expect(toast.toasts().length).toBe(0);
  });

  it("keeps the end page honest and flips it to Queue clear only when the pager agrees", async () => {
    const { screen, reader } = await setup([judgment(9, "A judgment")]);
    const end = reader.querySelector('[data-slot="queue-end"]') as HTMLElement;
    // With an open judgment the end page NEVER claims clear — it derives from
    // the same openCount predicate as the pager's red pill.
    expect(end.textContent).toContain("End of the queue");
    expect(end.textContent).toContain("1 judgment above still needs you");
    expect(end.textContent).not.toContain("Queue clear");
    // Decide it: both surfaces flip together — pager "all clear", page "Queue
    // clear" with an honest 0-waiting tally.
    fireEvent.click(screen.getByRole("button", { name: /Pick A for 9/ }));
    await waitFor(() => expect(end.textContent).toContain("Queue clear"));
    const need = screen.container.querySelector('[data-slot="pager-need"]') as HTMLElement;
    expect(need.textContent).toBe("all clear");
    expect(end.textContent).toContain("1 decided");
    expect(end.textContent).toContain("0 waiting");
  });

  it("opens the phone peek overview from the pager", async () => {
    const { screen } = await setup([judgment(9, "A judgment")]);
    // Tap the pager to peek the whole queue.
    fireEvent.click(screen.getByLabelText("Open queue overview"));
    const peek = await waitFor(
      () => screen.container.querySelector('[data-slot="event-peek"]') as HTMLElement,
    );
    // The peek lists the event by its @-handle + derived title (same QueueRow as
    // the standing list).
    expect(within(peek).getByText("@j9")).toBeTruthy();
    expect(within(peek).getByText("A judgment")).toBeTruthy();
  });

  it("focuses the scroller root so the keyboard is alive on load (§5)", async () => {
    const { reader } = await setup([judgment(1, "One")]);
    await waitFor(() => expect(document.activeElement).toBe(reader));
  });

  it("lets Space/Enter activate a focused button instead of paging (§5)", async () => {
    const { screen, reader } = await setup([judgment(1, "One"), judgment(2, "Two")]);
    // Focus a real button in the reader: the handler must bail so the browser can
    // activate it (WAI-ARIA button semantics), never page the scroller.
    const next = screen.getAllByLabelText("Next event")[0] as HTMLElement;
    next.focus();
    const onButton = new KeyboardEvent("keydown", { key: " ", bubbles: true, cancelable: true });
    reader.dispatchEvent(onButton);
    expect(onButton.defaultPrevented).toBe(false);
    // With focus on the plain scroller root (not a control), Space DOES page.
    reader.focus();
    const onRoot = new KeyboardEvent("keydown", { key: " ", bubbles: true, cancelable: true });
    reader.dispatchEvent(onRoot);
    expect(onRoot.defaultPrevented).toBe(true);
  });

  it("carries the judgment into Chat as a quoted prefill on Discuss (§4)", async () => {
    const { store, screen } = await setup([judgment(7, "Wire the reopen op")]);
    fireEvent.click(screen.getAllByRole("button", { name: /Discuss/ })[0]);
    // Drilled into Chat with the composer pre-seeded — the judgment is never lost.
    expect(store.state.home).toBe("chat");
    const prefill = store.state.composerPrefill ?? "";
    expect(prefill.startsWith(">")).toBe(true);
    expect(prefill).toContain("@j7");
    expect(prefill).toContain("Wire the reopen op");
  });

  it("snoozes to the end with an Undo toast (§6)", async () => {
    const { toast, reader } = await setup([judgment(1, "One"), judgment(2, "Two")]);
    reader.focus();
    reader.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true, cancelable: true }));
    expect(toast.toasts().some((t) => /Snoozed/.test(t.message))).toBe(true);
    expect(toast.toasts().some((t) => t.action?.label === "Undo")).toBe(true);
  });
});

describe("EventScroller — hello_ok snapshot replacement (live-stack regression)", () => {
  function summaryEvent(id: number, text: string): EventItem {
    return {
      id,
      kind: "summary",
      source: { kind: "scheduled", ref: "digest" },
      name: `@s${id}`,
      description: text,
      requires_response: false,
      quick_replies: [],
      status: "open",
      read: false,
      anchor: 0,
      ts: `2026-07-13T0${id}:30:00Z`,
      ui: [{ type: "text", text }],
    };
  }

  it("re-anchors to the first needs-you card with zero decided/read residue", async () => {
    // Mirror the live sequence exactly: mount against an EMPTY store — the DEV
    // mock seed fires — then the real host's hello_ok replaces the set with one
    // open unread judgment and one open unread summary ~a moment later.
    const store = await import("../../store/store");
    vi.doMock("../../ws/client", () => ({ getClient: () => ({ sendEventAction: () => {} }) }));
    const { EventScroller } = await import("./EventScroller");
    const screen = render(() => <EventScroller />);
    // The DEV mocks are on screen (the pre-cutover demo pass).
    expect(store.state.events.length).toBeGreaterThan(0);

    store.dispatch({
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: 0,
        messages: [],
        pings: [],
        events: [judgment(1, "Persist the canvas?"), summaryEvent(2, "Morning digest")],
      },
    });

    // (a) The visible page is the JUDGMENT — the first needs-you card — not the
    // clear page the browser-preserved offset used to land on.
    await waitFor(() => {
      const pos = screen.container.querySelector('[data-slot="pager-pos"]') as HTMLElement;
      expect(pos.textContent).toBe("1 of 2");
    });
    // (b) The swap left no session residue: nothing decided, nothing read — the
    // mock pass and the initial jump never count as "passed" pages.
    expect(store.state.eventDecideOverrides).toEqual([]);
    expect(store.state.events.map((e) => e.id).sort()).toEqual([1, 2]);
    expect(store.state.events.every((e) => !e.read)).toBe(true);
    // (c) The pager and the end page derive from the SAME predicate and agree:
    // one open judgment → the red "1 need you", and the end page says so too —
    // no "Queue clear", no phantom decided/read chips.
    const need = screen.container.querySelector('[data-slot="pager-need"]') as HTMLElement;
    expect(need.textContent).toBe("1 need you");
    const end = screen.container.querySelector('[data-slot="queue-end"]') as HTMLElement;
    expect(end.textContent).toContain("End of the queue");
    expect(end.textContent).toContain("1 judgment above still needs you");
    expect(end.textContent).not.toContain("Queue clear");
    expect(end.textContent).not.toContain("decided");
    expect(end.textContent).not.toContain("read");
  });

  it("marks awareness read only when the Owner actually leaves its page", async () => {
    const { store, reader } = await setup([judgment(1, "One")]);
    // Append a summary AFTER mount (an event_upsert, not a snapshot swap).
    store.dispatch({
      type: "event_upsert",
      payload: { type: "event_upsert", event: summaryEvent(2, "Digest") },
    });
    expect(store.state.events.find((e) => e.id === 2)?.read).toBe(false);
    reader.focus();
    // Page down onto the summary: it is CENTRED, not passed — still unread.
    reader.dispatchEvent(new KeyboardEvent("keydown", { key: "j", bubbles: true, cancelable: true }));
    expect(store.state.events.find((e) => e.id === 2)?.read).toBe(false);
    // Page past it: the Owner actually viewed it and left — now it reads.
    reader.dispatchEvent(new KeyboardEvent("keydown", { key: "j", bubbles: true, cancelable: true }));
    await waitFor(() => expect(store.state.events.find((e) => e.id === 2)?.read).toBe(true));
  });
});
