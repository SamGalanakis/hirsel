import { fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { EventItem } from "../../protocol";

// The desktop Feed column (desktop-unified). The queue's honest "expanded mobile"
// form: a scrollable COLUMN of the same decidable cards (not the phone's
// full-viewport pager, not the old index+reader split), decided inline, with
// "Discuss" dropping a quoted reference into the STANDING composer beside it —
// no navigation, since Chat is always on screen. Fresh store singleton per test.
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
      readEvent: () => {},
    }),
  }));
  // Seed BEFORE render so the DEV mock-seed (empty-set only) never interferes.
  for (const e of events) {
    store.dispatch({ type: "event_upsert", payload: { type: "event_upsert", event: e } });
  }
  const { FeedColumn } = await import("./FeedColumn");
  const screen = render(() => <FeedColumn />);
  const column = screen.container.querySelector('[data-slot="feed-column"]') as HTMLElement;
  return { store, screen, sent, column };
}

describe("FeedColumn — the desktop card column", () => {
  it("renders a card per event, blocking-first, with the surface's one red count", async () => {
    const { column } = await setup([
      judgment(1, "Second judgment"),
      judgment(2, "Blocking judgment", true),
    ]);
    // Both cards render as full decidable cards in the column.
    expect(within(column).getByText("Blocking judgment")).toBeTruthy();
    expect(within(column).getByText("Second judgment")).toBeTruthy();
    // The header pill is the one red — the needs-you count in danger tone.
    const need = column.querySelector('[data-slot="feed-need"]') as HTMLElement;
    expect(need.textContent).toBe("2 need you");
    expect(need.className).toContain("status-danger");
    // The blocking judgment sorts above the other (rank 0 before rank 1).
    const cards = within(column).getAllByText(/judgment$/);
    expect(cards[0].textContent).toContain("Blocking judgment");
    // Decruft: no "Choose an option to decide" filler — the options ARE the
    // affordance; only the Discuss escape hatch remains in the footer.
    expect(within(column).queryByText(/Choose an option/i)).toBeNull();
    expect(within(column).getAllByRole("button", { name: /Discuss/ }).length).toBeGreaterThan(0);
  });

  it("decides inline: taps an option, posts the event_action, flips to the decided strip", async () => {
    const { screen, sent, column } = await setup([judgment(5, "Only judgment")]);
    fireEvent.click(screen.getByRole("button", { name: /Pick A for 5/ }));
    // The exact wire action, and the card flips to its on-card decided strip
    // (which carries the single Undo — no toast stacked on top).
    expect(sent).toEqual([{ eventId: 5, action: "choose", data: { choice: "A", label: "Pick A for 5" } }]);
    await waitFor(() => expect(within(column).getByText("decided")).toBeTruthy());
    expect(within(column).getByRole("button", { name: "Undo" })).toBeTruthy();
    // The count clears to all-clear once nothing is owed.
    const need = column.querySelector('[data-slot="feed-need"]') as HTMLElement;
    expect(need.textContent).toBe("all clear");
  });

  it("Discuss prefills the STANDING composer without navigating (Chat is already on screen)", async () => {
    const { store, screen } = await setup([judgment(7, "Wire the reopen op")]);
    fireEvent.click(screen.getByRole("button", { name: /Discuss/ }));
    // No navigation: home stays on the queue root — Discuss is not a drill-in on
    // desktop, it just seeds the always-mounted composer with a quoted reference.
    expect(store.state.home).toBe("queue");
    const prefill = store.state.composerPrefill ?? "";
    expect(prefill.startsWith(">")).toBe(true);
    expect(prefill).toContain("@j7");
    expect(prefill).toContain("Wire the reopen op");
  });

  it("default-hides archived events: cards and the needs-you count both read the filtered set", async () => {
    const archivedJudgment: EventItem = {
      ...judgment(3, "Already archived"),
      status: "done",
      archived: true,
    };
    const { column } = await setup([judgment(1, "Live judgment"), archivedJudgment]);
    // The archived card never renders in the resting column…
    expect(within(column).queryByText("Already archived")).toBeNull();
    expect(within(column).getByText("Live judgment")).toBeTruthy();
    // …and the count is honest against the filtered set.
    const need = column.querySelector('[data-slot="feed-need"]') as HTMLElement;
    expect(need.textContent).toBe("1 need you");
  });

  it("a host-archived OPEN judgment leaves the needs-you count (agent events.archive)", async () => {
    const { store, column } = await setup([judgment(1, "First"), judgment(2, "Second")]);
    const need = column.querySelector('[data-slot="feed-need"]') as HTMLElement;
    expect(need.textContent).toBe("2 need you");
    // The host archives an open judgment out from under the client (the agent
    // tool path) — still status open on this frame; archived alone must sweep it.
    store.dispatch({
      type: "event_upsert",
      payload: { type: "event_upsert", event: { ...judgment(2, "Second"), archived: true } },
    });
    await waitFor(() => expect(within(column).queryByText("Second")).toBeNull());
    expect(need.textContent).toBe("1 need you");
  });

  it("Archive on the decided strip posts the contract envelope and animates the card out", async () => {
    const { screen, sent, column } = await setup([judgment(5, "Only judgment")]);
    fireEvent.click(screen.getByRole("button", { name: /Pick A for 5/ }));
    // The decided strip carries the natural "done with this" exit next to Undo.
    const archive = await waitFor(
      () => within(column).getByRole("button", { name: "Archive" }),
    );
    fireEvent.click(archive);
    // The exact contract envelope: action archive, data {} (not null).
    await waitFor(() =>
      expect(sent).toContainEqual({ eventId: 5, action: "archive", data: {} }),
    );
    // The card leaves the resting column (after the motion-safe exit).
    await waitFor(() => expect(within(column).queryByText("Only judgment")).toBeNull());
  });

  it("a finished awareness card carries the ⋯ overflow Archive; an open judgment never does", async () => {
    const readSummary: EventItem = {
      ...judgment(6, "Read digest"),
      kind: "summary",
      requires_response: false,
      read: true,
    };
    const { sent, column } = await setup([judgment(1, "Open judgment"), readSummary]);
    // The open judgment offers no overflow — deciding is how it leaves the queue.
    expect(within(column).queryByLabelText("More actions for @j1")).toBeNull();
    const user = userEvent.setup();
    await user.click(within(column).getByLabelText("More actions for @j6"));
    await user.click(await within(document.body).findByRole("menuitem", { name: "Archive" }));
    await waitFor(() =>
      expect(sent).toContainEqual({ eventId: 6, action: "archive", data: {} }),
    );
    await waitFor(() => expect(within(column).queryByText("Read digest")).toBeNull());
  });

  it("Archived filter discloses dense rows; Unarchive returns the event to the feed", async () => {
    const archivedSummary: EventItem = {
      ...judgment(4, "Quarterly digest"),
      kind: "summary",
      requires_response: false,
      status: "done",
      read: true,
      archived: true,
    };
    const { sent, column } = await setup([judgment(1, "Live judgment"), archivedSummary]);
    // Default Active every session: no archived rows standing.
    expect(column.querySelector('[data-slot="feed-archived"]')).toBeNull();
    // The segmented control's Archived chip carries the honest count.
    const chip = within(column).getByRole("button", { name: /Archived/ });
    expect(chip.getAttribute("aria-pressed")).toBe("false");
    expect(chip.textContent).toContain("1");
    fireEvent.click(chip);
    // Dense rows — @name + title + Unarchive — never full cards.
    const section = column.querySelector('[data-slot="feed-archived"]') as HTMLElement;
    expect(within(section).getByText("@j4")).toBeTruthy();
    expect(within(section).queryByRole("button", { name: /Pick A/ })).toBeNull();
    const unarchive = within(section).getByRole("button", { name: /Unarchive @j4/ });
    fireEvent.click(unarchive);
    // The exact contract envelope, and — the archive now empty — the column
    // falls back to Active, where the returned event stands as a card again.
    expect(sent).toContainEqual({ eventId: 4, action: "unarchive", data: {} });
    await waitFor(() => expect(within(column).getByText("Quarterly digest")).toBeTruthy());
    expect(column.querySelector('[data-slot="feed-archived"]')).toBeNull();
  });

  it("search narrows the cards live; Needs you keeps only open judgments", async () => {
    const { screen, column } = await setup([judgment(1, "Wire the reopen op"), judgment(2, "Ship the digest")]);
    // Live search across handle/description/title.
    const search = within(column).getByLabelText("Search the queue") as HTMLInputElement;
    fireEvent.input(search, { target: { value: "reopen" } });
    expect(within(column).getByText("Wire the reopen op")).toBeTruthy();
    expect(within(column).queryByText("Ship the digest")).toBeNull();
    fireEvent.click(within(column).getByLabelText("Clear search"));
    expect(within(column).getByText("Ship the digest")).toBeTruthy();
    // Needs you: a decided judgment drops out of the narrowed set (it is no
    // longer owed) while the open one stays.
    fireEvent.click(screen.getByRole("button", { name: /Pick A for 2/ }));
    fireEvent.click(within(column).getByRole("button", { name: "Needs you" }));
    await waitFor(() => expect(within(column).queryByText("Ship the digest")).toBeNull());
    expect(within(column).getByText("Wire the reopen op")).toBeTruthy();
  });

  it("shows the inbox-zero empty state when the queue is genuinely clear", async () => {
    const { store, screen } = await setup([judgment(1, "One")]);
    // Authoritative empty snapshot clears the set (and any DEV seed).
    store.dispatch({
      type: "hello_ok",
      payload: { type: "hello_ok", latest_msg_id: 0, messages: [], pings: [], events: [] },
    });
    await waitFor(() => expect(screen.getByText("Nothing needs you")).toBeTruthy());
  });
});
