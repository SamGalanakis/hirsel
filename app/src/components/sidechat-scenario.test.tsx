import { fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ChatMessage, InboxItem } from "../protocol";

// Headless full-loop scenario for the Side Chat sheet (ADR-0008 / the opus
// design critique), driven the same way as the rest of this suite's scenario
// tests (flows.test.tsx, inbox/inbox-reply-scenario.test.tsx,
// processes/processes-scenario.test.tsx): a pristine store per test
// (resetModules + dynamic import) and a fake ws client that reproduces the
// mock server's v2.0 behavior via direct `dispatch` calls instead of a live
// socket — the actual tools/mock-server.mjs is exercised separately (see the
// scratchpad smoke test run against a live instance), this is the client-side
// contract against that same protocol.
beforeEach(() => {
  vi.resetModules();
});

function inboxItem(overrides: Partial<InboxItem> = {}): InboxItem {
  return {
    id: 1,
    content: "Approve the deploy to prod?",
    anchor: 5,
    requires_response: true,
    quick_replies: [],
    status: "open",
    ts: "2026-07-08T00:00:00Z",
    ...overrides,
  };
}

function anchorMessage(): ChatMessage {
  return {
    id: 5,
    author: "agent",
    body: "Should I deploy to prod now?",
    ref: null,
    ts: "2026-07-08T00:00:00Z",
  };
}

/** A fake host reproducing the v2.0 side-chat surface closely enough to drive
 * the full loop: idempotent open/resume (remembers transcripts across a
 * leave+resume so "resume" actually restores history), scoped send/reply,
 * conclude -> draft, confirm -> main owner msg + archive + closed, and
 * discard. */
function makeFakeHost(store: typeof import("../store/store")) {
  let scCounter = 0;
  let sideMsgId = 100;
  const byItem = new Map<number, string>();
  const transcripts = new Map<string, ChatMessage[]>();

  return {
    openSideChat: vi.fn((itemId: number) => {
      let sc = byItem.get(itemId);
      if (!sc) {
        sc = `side:${++scCounter}`;
        byItem.set(itemId, sc);
        transcripts.set(sc, []);
      }
      store.dispatch({ type: "side_chat_open", sc, itemId, messages: transcripts.get(sc) ?? [] });
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
      // Echo + a scripted scoped reply, mirroring the mock server: owner echo
      // first (reconciles the optimistic bubble), then the agent's turn. The
      // turn's commit is deferred a tick (like the real mock server's actual
      // delay) so there's a real window to observe the live sc-scoped
      // thinking/timeline before it clears on commit, instead of both landing
      // in the same synchronous batch.
      const owner: ChatMessage = { id: sideMsgId++, author: "owner", body, ref, ts: "2026-07-08T00:01:00Z" };
      store.dispatch({ type: "side_chat_msg", sc, message: owner });
      transcripts.get(sc)?.push(owner);

      store.dispatch({ type: "side_chat_agent_activity", sc, state: "thinking", text: "Thinking…" });
      store.dispatch({
        type: "side_chat_turn_event",
        sc,
        seq: 1,
        event: { kind: "prose", text: "Let me check the recent context. " },
      });
      setTimeout(() => {
        const reply: ChatMessage = {
          id: sideMsgId++,
          author: "agent",
          body: "Looks safe to ship — tests are green.",
          ref: owner.id,
          ts: "2026-07-08T00:01:05Z",
        };
        store.dispatch({ type: "side_chat_msg", sc, message: reply });
        transcripts.get(sc)?.push(reply);
      }, 10);
      return localId;
    }),
    cancelSideTurn: vi.fn(),
    concludeSideChat: vi.fn((sc: string) => {
      store.dispatch({ type: "side_chat_conclude_requested", sc });
      store.dispatch({
        type: "side_chat_conclusion_draft",
        sc,
        text: "Approving — tests are green, go ahead and ship.",
      });
    }),
    confirmConclusion: vi.fn((sc: string, text: string, anchor: number) => {
      store.dispatch({ type: "side_chat_confirm_sent", sc, anchor });
      // Host's confirm_conclusion handling: normal msg flow in MAIN chat,
      // idempotent archive, then discard the session.
      store.dispatch({
        type: "msg",
        payload: {
          type: "msg",
          message: { id: 900, author: "owner", body: text, ref: anchor, ts: "2026-07-08T00:02:00Z" },
        },
      });
      const itemId = byItem.entries().next().value?.[0];
      const item = store.state.inbox.find((i) => i.anchor === anchor);
      if (item) {
        store.dispatch({
          type: "inbox_upsert",
          payload: { type: "inbox_upsert", item: { ...item, status: "archived" } },
        });
      }
      void itemId;
      store.dispatch({ type: "side_chat_closed", sc });
    }),
    discardSideChat: vi.fn((sc: string) => {
      store.dispatch({ type: "side_chat_closed", sc });
    }),
    archiveItem: vi.fn(),
    readItem: vi.fn(),
    sendMessage: vi.fn(() => -1),
  };
}

async function setup(itemOverrides: Partial<InboxItem> = {}) {
  const store = await import("../store/store");
  const fakeClient = makeFakeHost(store);
  vi.doMock("../ws/client", () => ({ getClient: () => fakeClient }));

  const { ChatView } = await import("./chat/ChatView");
  store.dispatch({ type: "connection_status", status: "connected" });
  store.dispatch({ type: "msg", payload: { type: "msg", message: anchorMessage() } });
  store.dispatch({
    type: "inbox_upsert",
    payload: { type: "inbox_upsert", item: inboxItem(itemOverrides) },
  });
  const screen = render(() => <ChatView />);
  return { store, screen, fakeClient };
}

/** Scope queries to the sheet itself: the main Composer sits mounted (just
 * covered, not unmounted) behind the sheet's fixed overlay, and shares its
 * "Message the Agent…" placeholder / "Send" label with the side composer. */
function sideSheet() {
  const el = document.querySelector('[data-slot="side-chat-sheet"]');
  if (!el) throw new Error("side chat sheet is not open");
  return within(el as HTMLElement);
}

describe("Full loop: item -> Discuss -> side conversation -> Conclude -> Send reply -> landed in main", () => {
  it("keeps main chat untouched during the side conversation, then lands the edited conclusion with its provenance chip and archives the item", async () => {
    const { store, screen, fakeClient } = await setup();

    // Main chat starts with just the anchor message.
    expect(store.state.messages).toHaveLength(1);

    // --- Discuss ---
    await fireEvent.click(screen.getByLabelText("Open inbox"));
    await fireEvent.click(screen.getByRole("button", { name: /Discuss/ }));
    expect(fakeClient.openSideChat).toHaveBeenCalledWith(1);

    // --- Seed card visible ---
    await waitFor(() => expect(screen.getByText(/Forked from your chat/)).toBeTruthy());
    expect(screen.getByText(/Side chat ·/)).toBeTruthy();
    // the seeded item content, rendered in both the title band and the seed
    // card body (plus behind, in the still-expanded Tray card — hence scoping
    // to the sheet itself, and getAllByText since it legitimately appears
    // twice within the sheet).
    expect(sideSheet().getAllByText(/Approve the deploy to prod\?/).length).toBeGreaterThan(0);

    // --- Side conversation: sc-scoped timeline visible, main chat untouched ---
    const composer = sideSheet().getByPlaceholderText("Message the Agent…") as HTMLTextAreaElement;
    fireEvent.input(composer, { target: { value: "What do you think?" } });
    await fireEvent.click(sideSheet().getByLabelText("Send"));

    expect(fakeClient.sendSideMessage).toHaveBeenCalledWith("side:1", "What do you think?", null);
    // The scoped prose delta from the side turn is visible while it's still
    // running, and never touches main state (the structural guarantee).
    await waitFor(() => expect(screen.getByText(/Let me check the recent context\./)).toBeTruthy());
    expect(store.state.turnEvents).toEqual([]);
    expect(store.state.messages).toHaveLength(1);

    await waitFor(() => expect(screen.getByText("Looks safe to ship — tests are green.")).toBeTruthy());
    // Main chat is still untouched behind the sheet after the side turn commits.
    expect(store.state.messages).toHaveLength(1);
    expect(store.state.turnEvents).toEqual([]);

    // --- Conclude -> edit draft -> Send reply ---
    await fireEvent.click(screen.getByRole("button", { name: "Conclude" }));
    expect(fakeClient.concludeSideChat).toHaveBeenCalledWith("side:1");

    const textarea = (await screen.findByLabelText("Reply text")) as HTMLTextAreaElement;
    expect(textarea.value).toBe("Approving — tests are green, go ahead and ship.");
    fireEvent.input(textarea, { target: { value: "Approved — ship it now, tests are green." } });

    const confirmDialog = screen.getByRole("dialog", { name: "Send this reply?" });
    await fireEvent.click(within(confirmDialog).getByRole("button", { name: "Send reply" }));

    expect(fakeClient.confirmConclusion).toHaveBeenCalledWith(
      "side:1",
      "Approved — ship it now, tests are green.",
      5,
    );

    // --- Sheet closes, land-and-highlight, provenance chip, item archived ---
    await waitFor(() => expect(screen.queryByText(/Side chat ·/)).toBeNull());
    expect(store.state.messages).toHaveLength(2);
    const owner = store.state.messages.find((m) => m.id === 900);
    expect(owner).toMatchObject({ body: "Approved — ship it now, tests are green.", ref: 5 });
    expect(store.state.conclusionChips).toContain(900);

    const ownerBubbleText = screen.getByText("Approved — ship it now, tests are green.");
    expect(ownerBubbleText).toBeTruthy();
    expect(screen.getByText("worked out in a side chat")).toBeTruthy();

    const ownerBubbleContent = document.getElementById("msg-900")?.querySelector('[data-slot="bubble-content"]');
    expect(ownerBubbleContent?.className).toContain("ring-2");

    expect(store.state.inbox.find((i) => i.id === 1)?.status).toBe("archived");
  });
});

describe("Resume after reconnect", () => {
  it("shows 'in progress · resume' (never auto-opens), and resuming restores the prior transcript", async () => {
    const { store, screen, fakeClient } = await setup();
    await fireEvent.click(screen.getByLabelText("Open inbox"));
    await fireEvent.click(screen.getByRole("button", { name: /Discuss/ }));
    await waitFor(() => expect(screen.getByText(/Side chat ·/)).toBeTruthy());

    const composer = sideSheet().getByPlaceholderText("Message the Agent…") as HTMLTextAreaElement;
    fireEvent.input(composer, { target: { value: "Any concerns?" } });
    await fireEvent.click(sideSheet().getByLabelText("Send"));
    await waitFor(() => expect(screen.getByText("Looks safe to ship — tests are green.")).toBeTruthy());

    // Leave-alive: the side chat persists, the sheet just closes.
    await fireEvent.click(screen.getByLabelText(/Leave side chat/));
    expect(screen.queryByText(/Side chat ·/)).toBeNull();

    // A reconnect happens; the host still lists this side chat as live. The
    // sheet must NOT auto-reopen — only the tray card's affordance changes.
    store.dispatch({
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: store.state.lastSeenMsgId ?? 0,
        messages: [],
        inbox: store.state.inbox,
        side_chats: [{ sc: "side:1", item_id: 1 }],
      },
    });
    expect(screen.queryByText(/Side chat ·/)).toBeNull(); // still not auto-opened
    expect(screen.getByRole("button", { name: /in progress · resume/ })).toBeTruthy();

    // Resume is a deliberate tap: it re-opens (idempotent) and restores history.
    await fireEvent.click(screen.getByRole("button", { name: /in progress · resume/ }));
    expect(fakeClient.openSideChat).toHaveBeenCalledTimes(2);
    await waitFor(() => expect(screen.getByText(/Side chat ·/)).toBeTruthy());
    // Appears twice, legitimately: the owner bubble itself, and the agent
    // reply's QuotedRef snippet quoting it back.
    expect(screen.getAllByText("Any concerns?").length).toBeGreaterThan(0);
    expect(screen.getByText("Looks safe to ship — tests are green.")).toBeTruthy();
  });
});

describe("Item archived mid-side-chat", () => {
  it("shows a non-blocking banner and Conclude still works", async () => {
    const { screen, fakeClient } = await setup();
    await fireEvent.click(screen.getByLabelText("Open inbox"));
    await fireEvent.click(screen.getByRole("button", { name: /Discuss/ }));
    await waitFor(() => expect(screen.getByText(/Side chat ·/)).toBeTruthy());

    expect(screen.queryByText(/The Agent closed this item\./)).toBeNull();

    // The Agent archives the item while the side chat is still open.
    const store = await import("../store/store");
    const item = store.state.inbox.find((i) => i.id === 1);
    store.dispatch({
      type: "inbox_upsert",
      payload: { type: "inbox_upsert", item: { ...item!, status: "archived" } },
    });

    expect(screen.getByText(/The Agent closed this item\./)).toBeTruthy();
    // The sheet itself is not killed, and Conclude is still reachable.
    expect(screen.getByText(/Side chat ·/)).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Conclude" }));
    expect(fakeClient.concludeSideChat).toHaveBeenCalledWith("side:1");
  });
});
