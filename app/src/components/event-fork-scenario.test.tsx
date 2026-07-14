import { fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ChatMessage, EventItem } from "../protocol";

// Headless full-loop scenario for the event-fork surface (ADR-0008 forks over
// ADR-0012 events, v2.4), driven like the rest of this suite's scenario tests: a
// pristine store per test (resetModules + dynamic import) and a fake ws client
// that reproduces the merged host contract via direct `dispatch` calls instead
// of a live socket. Covers the flagship loop — open by event, talk it through in
// the scoped thread (main chat untouched), decide from the pinned card, watch
// the fork close the loop and the "Discussed → …" owner line land in main — plus
// leave-alive + resume across a reconnect.
beforeEach(() => {
  vi.resetModules();
});

function judgment(overrides: Partial<EventItem> = {}): EventItem {
  return {
    id: 1,
    kind: "judgment",
    source: { kind: "agent", ref: "hirsel-host" },
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

function anchorMessage(): ChatMessage {
  return { id: 5, author: "agent", body: "Should I deploy to prod now?", ref: null, ts: "2026-07-08T00:00:00Z" };
}

/** A fake host reproducing the v2.4 event-fork surface: idempotent open/resume
 * (remembers transcripts across leave+resume), scoped send/reply, and a decide
 * (event_action{choose}) that resolves the event, clears fork_sc, posts the one
 * "Discussed @name → <label>" owner line in MAIN chat, and closes the fork. */
function makeFakeHost(store: typeof import("../store/store")) {
  let sideMsgId = 100;
  const transcripts = new Map<string, ChatMessage[]>([["side:1", []]]);

  return {
    openSideChat: vi.fn((eventId: number) => {
      store.dispatch({ type: "side_chat_open", sc: "side:1", pingId: eventId, messages: transcripts.get("side:1") ?? [] });
    }),
    sendSideMessage: vi.fn((sc: string, body: string, ref: number | null) => {
      const localId = -(1000 + sideMsgId);
      store.dispatch({
        type: "side_chat_send_local",
        sc,
        localId,
        clientId: `c${localId}`,
        body,
        ref,
        ts: "2026-07-08T00:01:00Z",
      });
      const owner: ChatMessage = { id: sideMsgId++, author: "owner", body, ref, ts: "2026-07-08T00:01:00Z" };
      store.dispatch({ type: "side_chat_msg", sc, message: owner });
      transcripts.get(sc)?.push(owner);
      store.dispatch({ type: "side_chat_agent_activity", sc, state: "thinking", text: "Thinking…" });
      store.dispatch({
        type: "side_chat_turn_event",
        sc,
        seq: 1,
        event: { kind: "prose", text: "Checking the recent context. " },
      });
      setTimeout(() => {
        const reply: ChatMessage = {
          id: sideMsgId++,
          author: "agent",
          body: "Tests are green — safe to ship.",
          ref: owner.id,
          ts: "2026-07-08T00:01:05Z",
        };
        store.dispatch({ type: "side_chat_msg", sc, message: reply });
        transcripts.get(sc)?.push(reply);
      }, 10);
      return localId;
    }),
    cancelSideTurn: vi.fn(),
    discardSideChat: vi.fn((sc: string) => {
      store.dispatch({ type: "side_chat_closed", sc });
    }),
    sendEventAction: vi.fn((eventId: number, _action: string, data: unknown) => {
      const ev = store.state.events.find((e) => e.id === eventId);
      const label =
        data && typeof data === "object" && "label" in data ? String((data as { label: string }).label) : "";
      // The host: resolve + clear fork_sc, post the single owner line in MAIN, close.
      if (ev) {
        store.dispatch({
          type: "event_upsert",
          payload: { type: "event_upsert", event: { ...ev, status: "done", fork_sc: null } },
        });
        store.dispatch({
          type: "msg",
          payload: {
            type: "msg",
            message: { id: 900, author: "owner", body: `Discussed @${ev.name} → ${label}`, ref: ev.anchor, ts: "2026-07-08T00:02:00Z" },
          },
        });
      }
      store.dispatch({ type: "side_chat_closed", sc: "side:1" });
    }),
    readEvent: vi.fn(),
    sendMessage: vi.fn(() => -1),
  };
}

async function setup(overrides: Partial<EventItem> = {}) {
  const store = await import("../store/store");
  const fakeClient = makeFakeHost(store);
  vi.doMock("../ws/client", () => ({ getClient: () => fakeClient }));

  const { ChatView } = await import("./chat/ChatView");
  store.dispatch({ type: "connection_status", status: "connected" });
  store.dispatch({ type: "msg", payload: { type: "msg", message: anchorMessage() } });
  store.dispatch({
    type: "event_upsert",
    payload: { type: "event_upsert", event: { ...judgment(overrides), fork_sc: "side:1" } },
  });
  store.dispatch({ type: "side_chat_open", sc: "side:1", pingId: 1, messages: [] });
  store.openSideChat("side:1");
  const screen = render(() => <ChatView />);
  return { store, screen, fakeClient };
}

function panel() {
  const el = document.querySelector('[data-slot="side-chat-sheet"]');
  if (!el) throw new Error("event-fork panel is not open");
  return within(el as HTMLElement);
}

describe("Full loop: open by event → discuss → decide from the pinned card → close-the-loop", () => {
  it("keeps main chat untouched during the discussion, then decides from the pinned card and lands the 'Discussed →' owner line", async () => {
    const { store, screen, fakeClient } = await setup();

    // Main chat starts with just the anchor.
    expect(store.state.messages).toHaveLength(1);
    // Pinned card visible — THE card (its own ui), decidable.
    await waitFor(() => expect(screen.getByText(/Discussing/)).toBeTruthy());
    expect(panel().getByText("Approve the deploy to prod?")).toBeTruthy();

    // Scoped discussion: prose delta shows while running, main untouched.
    const composer = panel().getByPlaceholderText("Reply in this discussion…") as HTMLTextAreaElement;
    fireEvent.input(composer, { target: { value: "any risks?" } });
    await fireEvent.click(panel().getByLabelText("Send"));
    expect(fakeClient.sendSideMessage).toHaveBeenCalledWith("side:1", "any risks?", null);
    await waitFor(() => expect(screen.getByText(/Checking the recent context\./)).toBeTruthy());
    expect(store.state.messages).toHaveLength(1);
    expect(store.state.turnEvents).toEqual([]);
    await waitFor(() => expect(screen.getByText("Tests are green — safe to ship.")).toBeTruthy());
    expect(store.state.messages).toHaveLength(1);

    // Decide from the pinned card.
    fireEvent.click(panel().getByRole("button", { name: /Ship it/ }));
    expect(fakeClient.sendEventAction).toHaveBeenCalledWith(1, "choose", { choice: "A", label: "Ship it" });

    // The event resolves, fork_sc clears, the one owner line lands in MAIN chat…
    await waitFor(() => expect(store.state.events.find((e) => e.id === 1)?.status).toBe("done"));
    expect(store.state.events.find((e) => e.id === 1)?.fork_sc).toBeNull();
    await waitFor(() => expect(store.state.messages).toHaveLength(2));
    expect(store.state.messages.find((m) => m.id === 900)).toMatchObject({
      body: "Discussed @deploy-approval → Ship it",
      ref: 5,
    });

    // …and the fork closes the loop (the pane dismisses, region idle).
    await waitFor(
      () => expect(document.querySelector('[data-slot="side-chat-sheet"]')).toBeNull(),
      { timeout: 2500 },
    );
    expect(store.state.rightRegion).toBe("none");
    expect(store.state.activeSideChatSc).toBeNull();
  });
});

describe("Leave-alive and resume across a reconnect", () => {
  it("never auto-opens after reconnect, and resume restores the prior transcript", async () => {
    const { store, screen } = await setup();
    await waitFor(() => expect(screen.getByText(/Discussing/)).toBeTruthy());

    const composer = panel().getByPlaceholderText("Reply in this discussion…") as HTMLTextAreaElement;
    fireEvent.input(composer, { target: { value: "concerns?" } });
    await fireEvent.click(panel().getByLabelText("Send"));
    await waitFor(() => expect(screen.getByText("Tests are green — safe to ship.")).toBeTruthy());

    // Leave-alive: the fork persists, the pane just closes.
    await fireEvent.click(screen.getByLabelText(/Leave discussion/));
    expect(document.querySelector('[data-slot="side-chat-sheet"]')).toBeNull();

    // A reconnect still lists the fork as live — the pane must NOT auto-reopen.
    store.dispatch({
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: store.state.lastSeenMsgId ?? 0,
        messages: [],
        pings: [],
        events: [{ ...judgment(), fork_sc: "side:1" }],
        side_chats: [{ sc: "side:1", ping_id: 1 }],
      },
    });
    expect(document.querySelector('[data-slot="side-chat-sheet"]')).toBeNull();

    // Resume is a deliberate action and restores history.
    store.openSideChat("side:1");
    await waitFor(() => expect(screen.getByText(/Discussing/)).toBeTruthy());
    expect(panel().getAllByText("concerns?").length).toBeGreaterThan(0);
    expect(panel().getByText("Tests are green — safe to ship.")).toBeTruthy();
  });
});
