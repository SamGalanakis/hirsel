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
  const readSent: number[] = [];
  const opened: number[] = [];
  vi.doMock("../../ws/client", () => ({
    getClient: () => ({
      sendEventAction: (eventId: number, action: string, data: unknown) =>
        sent.push({ eventId, action, data }),
      openSideChat: (eventId: number) => opened.push(eventId),
      readEvent: (id: number) => readSent.push(id),
    }),
  }));
  for (const e of events) {
    store.dispatch({ type: "event_upsert", payload: { type: "event_upsert", event: e } });
  }
  const { EventScroller } = await import("./EventScroller");
  const screen = render(() => <EventScroller />);
  const reader = screen.container.querySelector('[data-slot="event-scroller"]') as HTMLElement;
  return { store, toast, screen, sent, readSent, opened, reader };
}

describe("EventScroller — the phone vertical event queue home", () => {
  it("shows the ONE red needs-you count and leads with the blocking judgment", async () => {
    const { screen, reader } = await setup([
      judgment(1, "Second judgment"),
      judgment(2, "Blocking judgment", true),
    ]);
    // The pager carries the surface's single red — two open judgments need you.
    const need = screen.container.querySelector('[data-slot="pager-need"]') as HTMLElement;
    expect(need.textContent).toBe("2 need you");
    expect(need.className).toContain("status-danger");
    // The reader card column renders the cards; the blocking one leads. (The
    // desktop two-column index is retired — desktop uses the Feed column now, so
    // the phone scroller no longer stands a standing list beside its reader.)
    expect(within(reader).getByText("Blocking judgment")).toBeTruthy();
    expect(within(reader).getByText("Second judgment")).toBeTruthy();
    expect(screen.container.querySelector('[data-slot="queue-list"]')).toBeNull();
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
    expect(need.className).toContain("bg-status-success/12");
    expect(need.className).toContain("text-foreground/80");
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
    // The clear branch carries the quiet intent door (§3): Talk to the agent →
    // the chat drill-in.
    const store = await import("../../store/store");
    fireEvent.click(within(end).getByRole("button", { name: /Talk to the agent/ }));
    expect(store.state.home).toBe("chat");
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

  it("default-hides archived events from the pages, the pager count, and the position", async () => {
    const archivedJudgment: EventItem = {
      ...judgment(2, "Archived judgment"),
      status: "done",
      archived: true,
    };
    const { screen, reader } = await setup([judgment(1, "Live judgment"), archivedJudgment]);
    // Only the live judgment pages; the archived one is filtered out everywhere.
    expect(within(reader).queryByText("Archived judgment")).toBeNull();
    const need = screen.container.querySelector('[data-slot="pager-need"]') as HTMLElement;
    expect(need.textContent).toBe("1 need you");
    const pos = screen.container.querySelector('[data-slot="pager-pos"]') as HTMLElement;
    expect(pos.textContent).toBe("1 of 1");
  });

  it("archives from the decided strip: envelope sent, page re-flows out, counts honest", async () => {
    const { screen, sent, reader } = await setup([judgment(3, "Ship it"), judgment(4, "Later one")]);
    fireEvent.click(screen.getByRole("button", { name: /Pick A for 3/ }));
    const archive = await waitFor(() =>
      within(reader).getByRole("button", { name: "Archive" }),
    );
    fireEvent.click(archive);
    await waitFor(() =>
      expect(sent).toContainEqual({ eventId: 3, action: "archive", data: {} }),
    );
    // The archived page leaves the pager: one page (the other judgment) remains.
    await waitFor(() => expect(within(reader).queryByText("Ship it")).toBeNull());
    const pos = screen.container.querySelector('[data-slot="pager-pos"]') as HTMLElement;
    expect(pos.textContent).toBe("1 of 1");
  });

  it("peek carries the Archived filter: default Active, dense rows, Unarchive returns it", async () => {
    const archivedSummary: EventItem = {
      ...judgment(5, "Old digest"),
      kind: "summary",
      requires_response: false,
      status: "done",
      read: true,
      archived: true,
    };
    const { screen, sent } = await setup([judgment(1, "Live judgment"), archivedSummary]);
    fireEvent.click(screen.getByLabelText("Open queue overview"));
    const peek = await waitFor(
      () => screen.container.querySelector('[data-slot="event-peek"]') as HTMLElement,
    );
    // Default Active on every open: no archived rows standing.
    expect(peek.querySelector('[data-slot="peek-archived"]')).toBeNull();
    expect(within(peek).queryByText("@j5")).toBeNull();
    fireEvent.click(within(peek).getByRole("button", { name: /Archived/ }));
    const section = peek.querySelector('[data-slot="peek-archived"]') as HTMLElement;
    expect(within(section).getByText("@j5")).toBeTruthy();
    // Unarchive posts the contract envelope; the archive empties, the peek falls
    // back to Active, and the pager behind gains the returned page.
    fireEvent.click(within(section).getByRole("button", { name: /Unarchive @j5/ }));
    expect(sent).toContainEqual({ eventId: 5, action: "unarchive", data: {} });
    await waitFor(() => expect(within(peek).getByText("@j5")).toBeTruthy());
    expect(peek.querySelector('[data-slot="peek-archived"]')).toBeNull();
    await waitFor(() => {
      const pos = screen.container.querySelector('[data-slot="pager-pos"]') as HTMLElement;
      expect(pos.textContent).toBe("1 of 2");
    });
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

  it("opens the event fork on Discuss — drills into the chat shell and registers the pending fork (§4)", async () => {
    const { store, screen, opened } = await setup([judgment(7, "Wire the reopen op")]);
    fireEvent.click(screen.getAllByRole("button", { name: /Discuss/ })[0]);
    // Supersedes the composer-prefill drill-in: a fresh open fires open_side_chat
    // by event id and drills into the chat shell (phone) so the fork panel mounts;
    // the pending fork opens the pane once side_chat_open lands the sc.
    expect(opened).toEqual([7]);
    expect(store.state.home).toBe("chat");
    expect(store.state.pendingSideChatPingId).toBe(7);
    expect(store.state.composerPrefill).toBeNull();
  });

  it("a durable-snoozed event leaves Active and returns via Unsnooze in the peek (Wave-3)", async () => {
    const { store, screen, reader } = await setup([judgment(1, "One"), judgment(2, "Two")]);
    const future = new Date(Date.now() + 6 * 3600_000).toISOString();
    // Snoozing (what the chooser's onPick does) sets snoozed_until: the event
    // leaves the pager entirely — counts stay honest.
    store.dispatch({ type: "event_snooze_local", eventId: 1, until: future });
    await waitFor(() => expect(within(reader).queryByText("One")).toBeNull());
    expect(within(reader).queryByText("Two")).toBeTruthy();
    // The peek discloses a Snoozed(1) filter; its row carries a durable Unsnooze.
    reader.focus();
    reader.dispatchEvent(new KeyboardEvent("keydown", { key: "p", bubbles: true }));
    const peek = await waitFor(() => {
      const el = screen.container.querySelector('[data-slot="event-peek"]') as HTMLElement | null;
      if (!el) throw new Error("peek not open");
      return el;
    });
    fireEvent.click(within(peek).getByRole("button", { name: /Snoozed/ }));
    const section = peek.querySelector('[data-slot="peek-snoozed"]') as HTMLElement;
    expect(within(section).getByRole("button", { name: /Unsnooze @j1/ })).toBeTruthy();
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
    vi.doMock("../../ws/client", () => ({
      getClient: () => ({ sendEventAction: () => {}, readEvent: () => {} }),
    }));
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

  it("marks awareness read only when the Owner actually leaves its page, and round-trips it on the wire", async () => {
    const { store, reader, readSent } = await setup([judgment(1, "One")]);
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
    // Page past it: the Owner actually viewed it and left — now it reads, and
    // the read is SENT (read_ping — events share the ping id space), so a
    // resync or a second device agrees instead of reverting the chip to "new".
    reader.dispatchEvent(new KeyboardEvent("keydown", { key: "j", bubbles: true, cancelable: true }));
    await waitFor(() => expect(store.state.events.find((e) => e.id === 2)?.read).toBe(true));
    expect(readSent).toEqual([2]);
  });

  it("streams awareness before a judgment without phantom-reading the summary", async () => {
    // The event_upsert path never bumps eventsSnapshotSeq: the summary arrives
    // first (the queue anchors on it as the only page), then a judgment sorts
    // ABOVE it. The summary was only ever displaced under the cursor — never
    // viewed-then-left — so it must stay unread with zero interaction.
    const { store, screen, readSent } = await setup([]);
    // Clear the DEV mock seed with an authoritative empty snapshot first.
    store.dispatch({
      type: "hello_ok",
      payload: { type: "hello_ok", latest_msg_id: 0, messages: [], pings: [], events: [] },
    });
    store.dispatch({
      type: "event_upsert",
      payload: { type: "event_upsert", event: summaryEvent(2, "Digest") },
    });
    store.dispatch({
      type: "event_upsert",
      payload: { type: "event_upsert", event: judgment(1, "Decide me") },
    });
    // The judgment leads; the pager agrees; the summary is untouched.
    await waitFor(() => {
      const pos = screen.container.querySelector('[data-slot="pager-pos"]') as HTMLElement;
      expect(pos.textContent).toBe("1 of 2");
    });
    const need = screen.container.querySelector('[data-slot="pager-need"]') as HTMLElement;
    expect(need.textContent).toBe("1 need you");
    expect(store.state.events.find((e) => e.id === 2)?.read).toBe(false);
    expect(readSent).toEqual([]);
  });
});
