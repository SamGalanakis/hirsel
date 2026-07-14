import { fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { EventItem } from "../../protocol";

// Component-level gates for the event-fork panel (ADR-0008 forks over ADR-0012
// events, v2.4): a host-opened fork renders THE event card pinned and decidable
// at the top; tapping an option on the pinned card posts the event_action and
// closes the loop; a summary fork has no decision, only a plain silent Close;
// leave-alive hides the panel without ending the fork; and the composer keeps
// its send/stop/paste parity. Same pristine-store-per-test pattern as the rest
// of the suite: resetModules + dynamic import so the freshly-imported
// store/components share one instance, and the ws client is mocked to synthesize
// the host's responses via direct `dispatch` calls instead of a live socket.
beforeEach(() => {
  vi.resetModules();
});

function judgment(overrides: Partial<EventItem> = {}): EventItem {
  return {
    id: 1,
    kind: "judgment",
    source: { kind: "agent", ref: "host" },
    name: "deploy-approval",
    description: "Approve the deploy to prod?",
    ui: [
      { type: "heading", text: "Approve the deploy to prod?" },
      {
        type: "optionList",
        action: "choose",
        options: [
          { key: "A", recommended: true, label: "Ship it" },
          { key: "B", label: "Hold" },
        ],
      },
    ],
    anchor: 5,
    requires_response: true,
    quick_replies: [],
    status: "open",
    read: false,
    ts: "2026-07-08T00:00:00Z",
    ...overrides,
  };
}

function summary(overrides: Partial<EventItem> = {}): EventItem {
  return {
    id: 2,
    kind: "summary",
    source: { kind: "scheduled", ref: "morning-digest" },
    name: "overnight-digest",
    description: "Overnight fleet digest",
    ui: [{ type: "text", text: "Overnight the fleet ran 6 turns across 3 repos." }],
    anchor: 0,
    requires_response: false,
    quick_replies: [],
    status: "open",
    read: false,
    ts: "2026-07-08T00:00:00Z",
    ...overrides,
  };
}

/** Mounts ChatView with a host-opened fork panel and a fake ws client that
 * behaves like the host's fork handling, synchronously. `sendEventAction` echoes
 * the decided event + closes the fork, mirroring the merged host contract. */
async function setup(ev: EventItem) {
  const store = await import("../../store/store");
  const sent: { eventId: number; action: string; data: unknown }[] = [];
  const fakeClient = {
    openSideChat: vi.fn(),
    sendSideMessage: vi.fn((sc: string, body: string, ref: number | null) => {
      store.dispatch({
        type: "side_chat_send_local",
        sc,
        localId: -1001,
        clientId: "c-1001",
        body,
        ref,
        ts: "2026-07-08T00:01:00Z",
      });
      return -1001;
    }),
    cancelSideTurn: vi.fn(),
    discardSideChat: vi.fn((sc: string) => {
      store.dispatch({ type: "side_chat_discard_sent", sc });
      store.dispatch({ type: "side_chat_closed", sc });
    }),
    sendEventAction: vi.fn((eventId: number, action: string, data: unknown) => {
      sent.push({ eventId, action, data });
      // The host resolves the event (fork_sc cleared) and closes the fork.
      const current = store.state.events.find((e) => e.id === eventId);
      if (current) {
        store.dispatch({
          type: "event_upsert",
          payload: { type: "event_upsert", event: { ...current, status: "done", fork_sc: null } },
        });
      }
      store.dispatch({ type: "side_chat_closed", sc: "side:1" });
    }),
    readEvent: vi.fn(),
    sendMessage: vi.fn(() => -1),
  };
  vi.doMock("../../ws/client", () => ({ getClient: () => fakeClient }));

  const { ChatView } = await import("../chat/ChatView");
  store.dispatch({ type: "connection_status", status: "connected" });
  store.dispatch({
    type: "event_upsert",
    payload: { type: "event_upsert", event: { ...ev, fork_sc: "side:1" } },
  });
  store.dispatch({ type: "side_chat_open", sc: "side:1", pingId: ev.id, messages: [] });
  store.openSideChat("side:1");
  const screen = render(() => <ChatView />);
  return { store, screen, sent, fakeClient };
}

function panel() {
  const el = document.querySelector('[data-slot="side-chat-sheet"]');
  if (!el) throw new Error("event-fork panel is not open");
  return within(el as HTMLElement);
}

describe("Event fork: the pinned, still-decidable card", () => {
  it("opens with THE event card pinned at the top — not a quote", async () => {
    const { screen } = await setup(judgment());
    await waitFor(() => expect(screen.getByText(/Discussing/)).toBeTruthy());
    // The pinned card renders the event's own ui (heading + options), THE card.
    expect(panel().getByText("Approve the deploy to prod?")).toBeTruthy();
    expect(panel().getByRole("button", { name: /Ship it/ })).toBeTruthy();
    expect(panel().getByRole("button", { name: /Hold/ })).toBeTruthy();
    // The @name titles the frame.
    expect(screen.getByText("@deploy-approval")).toBeTruthy();
  });

  it("decides from the pinned card: posts the event_action, shows the decided confirmation, then closes the loop", async () => {
    const { store, screen, sent } = await setup(judgment());
    await waitFor(() => expect(screen.getByText(/Discussing/)).toBeTruthy());

    fireEvent.click(panel().getByRole("button", { name: /Ship it/ }));
    // The exact wire action from the pinned card.
    expect(sent).toEqual([{ eventId: 1, action: "choose", data: { choice: "A", label: "Ship it" } }]);
    // The decided confirmation (DecidedStrip vocabulary) appears in place.
    await waitFor(() => expect(panel().getByText("decided")).toBeTruthy());
    // The event is resolved and its fork_sc cleared (chip would retire).
    expect(store.state.events.find((e) => e.id === 1)?.status).toBe("done");
    expect(store.state.events.find((e) => e.id === 1)?.fork_sc).toBeNull();
    // …then the panel closes the loop (the pane returns to idle).
    await waitFor(
      () => expect(document.querySelector('[data-slot="side-chat-sheet"]')).toBeNull(),
      { timeout: 2500 },
    );
    expect(store.state.rightRegion).toBe("none");
    expect(store.state.activeSideChatSc).toBeNull();
  });

  it("leaves the fork alive on the back gesture (resumable underneath)", async () => {
    const { store, screen } = await setup(judgment());
    await waitFor(() => expect(screen.getByText(/Discussing/)).toBeTruthy());
    await fireEvent.click(screen.getByLabelText(/Leave discussion/));
    expect(document.querySelector('[data-slot="side-chat-sheet"]')).toBeNull();
    // Region idle, but the sc stays set — the fork is resumable.
    expect(store.state.rightRegion).toBe("none");
    expect(store.state.activeSideChatSc).toBe("side:1");
  });
});

describe("Event fork: a summary fork closes silently", () => {
  it("has no decision — only a plain Close that discards without touching main chat", async () => {
    const { store, screen, fakeClient } = await setup(summary());
    await waitFor(() => expect(screen.getByText(/Discussing/)).toBeTruthy());
    // No decidable options on a summary's pinned card.
    expect(panel().queryByRole("button", { name: /Ship it|Hold/ })).toBeNull();

    const before = store.state.messages.length;
    await fireEvent.click(panel().getByRole("button", { name: "Close discussion" }));
    // Silent: discard, no main-chat message, the pane closes.
    expect(fakeClient.discardSideChat).toHaveBeenCalledWith("side:1");
    expect(store.state.messages.length).toBe(before);
    await waitFor(() =>
      expect(document.querySelector('[data-slot="side-chat-sheet"]')).toBeNull(),
    );
  });
});

describe("Event fork: composer parity", () => {
  it("sends a scoped reply on Cmd/Ctrl+Enter and shows a thinking-guarded Stop", async () => {
    const { store, screen, fakeClient } = await setup(judgment());
    await waitFor(() => expect(screen.getByText(/Discussing/)).toBeTruthy());

    const side = document.querySelector('[data-composer="side"]') as HTMLTextAreaElement;
    fireEvent.input(side, { target: { value: "what are the risks?" } });
    fireEvent.keyDown(side, { key: "Enter", ctrlKey: true });
    expect(fakeClient.sendSideMessage).toHaveBeenCalledWith("side:1", "what are the risks?", null);

    // Idle → no Stop; thinking → a Stop that cancels the scoped turn.
    expect(panel().queryByLabelText("Stop the agent")).toBeNull();
    store.dispatch({ type: "side_chat_agent_activity", sc: "side:1", state: "thinking", text: "…" });
    const stop = await panel().findByLabelText("Stop the agent");
    await fireEvent.click(stop);
    expect(fakeClient.cancelSideTurn).toHaveBeenCalledWith("side:1");
  });

  it("abandons a judgment fork from the ⋯ menu (Close discussion) without deciding", async () => {
    const { store, screen, fakeClient } = await setup(judgment());
    await waitFor(() => expect(screen.getByText(/Discussing/)).toBeTruthy());

    const user = userEvent.setup();
    await user.click(panel().getByLabelText("Discussion actions"));
    await user.click(await within(document.body).findByRole("menuitem", { name: "Close discussion" }));

    expect(fakeClient.discardSideChat).toHaveBeenCalledWith("side:1");
    // No decision was posted — the event stays open.
    expect(store.state.events.find((e) => e.id === 1)?.status).toBe("open");
    await waitFor(() =>
      expect(document.querySelector('[data-slot="side-chat-sheet"]')).toBeNull(),
    );
  });
});

describe("Focus handoff between the two composers", () => {
  it("lands focus in the fork composer on open and returns it to main on leave", async () => {
    const { screen } = await setup(judgment());
    await waitFor(() => expect(screen.getByText(/Discussing/)).toBeTruthy());

    const sideComposer = document.querySelector('[data-composer="side"]') as HTMLTextAreaElement;
    await waitFor(() => expect(document.activeElement).toBe(sideComposer));

    await fireEvent.click(screen.getByLabelText(/Leave discussion/));
    const mainComposer = document.querySelector('[data-composer="main"]') as HTMLTextAreaElement;
    await waitFor(() => expect(document.activeElement).toBe(mainComposer));
  });
});
