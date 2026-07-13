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

  it("renders the inbox-zero clear page and opens the phone peek overview", async () => {
    const { screen, reader } = await setup([judgment(9, "A judgment")]);
    expect(within(reader).getByText("Queue clear")).toBeTruthy();
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
