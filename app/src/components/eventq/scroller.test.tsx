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
  return { store, screen, sent };
}

describe("EventScroller — the vertical event queue home", () => {
  it("shows the ONE red needs-you count and leads with the blocking judgment", async () => {
    const { screen } = await setup([
      judgment(1, "Second judgment"),
      judgment(2, "Blocking judgment", true),
    ]);
    // The pager carries the single red — two open judgments need you.
    const need = screen.container.querySelector('[data-slot="pager-need"]') as HTMLElement;
    expect(need.textContent).toBe("2 need you");
    // Both cards render (the scroller is a pager, all pages mounted); the
    // blocking one leads the order.
    expect(screen.getByText("Blocking judgment")).toBeTruthy();
    expect(screen.getByText("Second judgment")).toBeTruthy();
  });

  it("posts an event_action, flips the card to decided, and drops the needs-you count", async () => {
    const { store, screen, sent } = await setup([judgment(5, "Only judgment")]);
    // Tap the recommended option.
    fireEvent.click(screen.getByRole("button", { name: /Pick A for 5/ }));
    // Optimistic decide + the exact wire action.
    expect(store.state.eventDecideOverrides).toContain(5);
    expect(sent).toEqual([{ eventId: 5, action: "choose", data: { choice: "A", label: "Pick A for 5" } }]);
    // The card flips to the decided strip and the count clears.
    await waitFor(() => expect(screen.getByText("decided")).toBeTruthy());
    const need = screen.container.querySelector('[data-slot="pager-need"]') as HTMLElement;
    expect(need.textContent).toBe("all clear");
  });

  it("renders the inbox-zero clear page and opens the peek overview", async () => {
    const { screen } = await setup([judgment(9, "A judgment")]);
    expect(screen.getByText("Queue clear")).toBeTruthy();
    // Tap the pager to peek the whole queue.
    fireEvent.click(screen.getByLabelText("Open queue overview"));
    const peek = await waitFor(
      () => screen.container.querySelector('[data-slot="event-peek"]') as HTMLElement,
    );
    // The peek lists the event by its @-handle + derived title.
    expect(within(peek).getByText("@j9")).toBeTruthy();
    expect(within(peek).getByText("A judgment")).toBeTruthy();
  });
});
