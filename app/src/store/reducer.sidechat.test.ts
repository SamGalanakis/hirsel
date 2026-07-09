import { describe, expect, it } from "vitest";
import { reduce } from "./reducer";
import { initialState } from "./types";
import type { ChatMessage, InboxItem } from "../protocol";

// v2.0 side chats (ADR-0008). Covers: sc-scoped routing never leaking into (or
// reading from) main state, side transcript accumulation/reconciliation, the
// full side-chat lifecycle (open/resume, conclude → confirm → closed,
// discard, host-initiated close), the client-derived conclusion-chip
// provenance, and the archived-mid-side-chat edge case. Same style as
// reducer.test.ts: pure `reduce` over `initialState()`, no store/DOM.

function msg(id: number, author: "owner" | "agent", body: string, ref: number | null = null): ChatMessage {
  return { id, author, body, ref, ts: `2026-07-08T00:00:0${id}Z` };
}

function inboxItem(overrides: Partial<InboxItem> = {}): InboxItem {
  return {
    id: 1,
    content: "hello",
    anchor: 1,
    requires_response: true,
    quick_replies: [],
    status: "open",
    ts: "2026-07-08T00:00:00Z",
    ...overrides,
  };
}

describe("side_chat_open", () => {
  it("creates a fresh scope with an empty transcript (the seed lives in the prompt layer)", () => {
    const state = reduce(initialState(), {
      type: "side_chat_open",
      sc: "side:1",
      itemId: 5,
      messages: [],
    });
    expect(state.sideChats["side:1"]).toMatchObject({
      sc: "side:1",
      itemId: 5,
      messages: [],
      drafting: false,
      draft: null,
      confirming: false,
      discarding: false,
      itemArchived: false,
      ended: false,
    });
    expect(state.sideChatRefs).toEqual([{ sc: "side:1", item_id: 5 }]);
  });

  it("is idempotent: resuming the same sc refreshes its transcript instead of creating a second entry", () => {
    const opened = reduce(initialState(), {
      type: "side_chat_open",
      sc: "side:1",
      itemId: 5,
      messages: [],
    });
    const withHistory = reduce(opened, {
      type: "side_chat_msg",
      sc: "side:1",
      message: msg(1, "owner", "hi"),
    });
    const resumed = reduce(withHistory, {
      type: "side_chat_open",
      sc: "side:1",
      itemId: 5,
      messages: [msg(1, "owner", "hi"), msg(2, "agent", "welcome back")],
    });
    expect(Object.keys(resumed.sideChats)).toEqual(["side:1"]);
    expect(resumed.sideChats["side:1"].messages.map((m) => m.id)).toEqual([1, 2]);
    expect(resumed.sideChatRefs).toEqual([{ sc: "side:1", item_id: 5 }]);
  });
});

describe("sc-scoped routing never leaks into (or reads from) main state", () => {
  it("side_chat_msg only ever touches sideChats[sc], never state.messages", () => {
    const opened = reduce(initialState(), {
      type: "side_chat_open",
      sc: "side:1",
      itemId: 5,
      messages: [],
    });
    const withMain = reduce(opened, {
      type: "msg",
      payload: { type: "msg", message: msg(1, "owner", "main chat message") },
    });
    const withSide = reduce(withMain, {
      type: "side_chat_msg",
      sc: "side:1",
      message: msg(1, "agent", "side chat message"),
    });

    expect(withSide.messages).toHaveLength(1);
    expect(withSide.messages[0].body).toBe("main chat message");
    expect(withSide.sideChats["side:1"].messages).toHaveLength(1);
    expect(withSide.sideChats["side:1"].messages[0].body).toBe("side chat message");
  });

  it("side_chat_agent_activity/turn_event never touch main agentActivity/turnEvents", () => {
    const opened = reduce(initialState(), {
      type: "side_chat_open",
      sc: "side:1",
      itemId: 5,
      messages: [],
    });
    const withSideActivity = reduce(opened, {
      type: "side_chat_agent_activity",
      sc: "side:1",
      state: "thinking",
      text: "Working…",
    });
    const withSideTurnEvent = reduce(withSideActivity, {
      type: "side_chat_turn_event",
      sc: "side:1",
      seq: 1,
      event: { kind: "prose", text: "hello" },
    });

    expect(withSideTurnEvent.agentActivity).toEqual({ state: "idle", text: null });
    expect(withSideTurnEvent.turnEvents).toEqual([]);
    expect(withSideTurnEvent.sideChats["side:1"].agentActivity).toEqual({
      state: "thinking",
      text: "Working…",
    });
    expect(withSideTurnEvent.sideChats["side:1"].turnEvents).toEqual([
      { seq: 1, event: { kind: "prose", text: "hello" } },
    ]);
  });

  it("an unknown/already-closed sc is a no-op, never touching any other state", () => {
    const state = reduce(initialState(), {
      type: "side_chat_msg",
      sc: "side:ghost",
      message: msg(1, "agent", "too late"),
    });
    expect(state).toEqual(initialState());
  });
});

describe("side transcript accumulation and reconciliation", () => {
  it("accumulates messages in order and reconciles an optimistic owner send against its echo", () => {
    const opened = reduce(initialState(), {
      type: "side_chat_open",
      sc: "side:1",
      itemId: 5,
      messages: [],
    });
    const withLocal = reduce(opened, {
      type: "side_chat_send_local",
      sc: "side:1",
      localId: -1,
      clientId: "c1",
      body: "what should I say",
      ref: null,
      ts: "2026-07-08T00:00:00Z",
    });
    expect(withLocal.sideChats["side:1"].messages).toEqual([
      expect.objectContaining({ id: -1, pending: true, clientId: "c1", body: "what should I say" }),
    ]);
    expect(withLocal.pendingSideSends).toEqual([
      { sc: "side:1", clientId: "c1", body: "what should I say", ref: null },
    ]);

    const echoed = reduce(withLocal, {
      type: "side_chat_msg",
      sc: "side:1",
      message: msg(101, "owner", "what should I say"),
    });
    expect(echoed.sideChats["side:1"].messages).toEqual([msg(101, "owner", "what should I say")]);
    expect(echoed.pendingSideSends).toEqual([]);

    const reply = reduce(echoed, {
      type: "side_chat_msg",
      sc: "side:1",
      message: msg(102, "agent", "Try mentioning the deadline."),
    });
    expect(reply.sideChats["side:1"].messages.map((m) => m.id)).toEqual([101, 102]);
  });

  it("clears the side turn timeline when the agent's message commits", () => {
    const opened = reduce(initialState(), {
      type: "side_chat_open",
      sc: "side:1",
      itemId: 5,
      messages: [],
    });
    const thinking = reduce(opened, {
      type: "side_chat_turn_event",
      sc: "side:1",
      seq: 1,
      event: { kind: "prose", text: "hmm" },
    });
    expect(thinking.sideChats["side:1"].turnEvents).toHaveLength(1);
    const committed = reduce(thinking, {
      type: "side_chat_msg",
      sc: "side:1",
      message: msg(1, "agent", "here's my take"),
    });
    expect(committed.sideChats["side:1"].turnEvents).toEqual([]);
  });
});

describe("side chat lifecycle states", () => {
  function openedSideChat() {
    return reduce(initialState(), {
      type: "side_chat_open",
      sc: "side:1",
      itemId: 5,
      messages: [],
    });
  }

  it("conclude → draft → keep editing returns to the composer without discarding", () => {
    const requested = reduce(openedSideChat(), {
      type: "side_chat_conclude_requested",
      sc: "side:1",
    });
    expect(requested.sideChats["side:1"].drafting).toBe(true);

    const drafted = reduce(requested, {
      type: "side_chat_conclusion_draft",
      sc: "side:1",
      text: "Approving this — looks good to ship.",
    });
    expect(drafted.sideChats["side:1"]).toMatchObject({
      drafting: false,
      draft: "Approving this — looks good to ship.",
    });

    const keptEditing = reduce(drafted, { type: "side_chat_keep_editing", sc: "side:1" });
    expect(keptEditing.sideChats["side:1"].draft).toBeNull();
    // Still open, not discarding/confirming/ended — the side chat is untouched.
    expect(keptEditing.sideChats["side:1"]).toMatchObject({
      discarding: false,
      confirming: false,
      ended: false,
    });
  });

  it("conclude → confirm_sent tracks the awaiting anchor, and side_chat_closed then drops the record", () => {
    const drafted = reduce(
      reduce(openedSideChat(), { type: "side_chat_conclude_requested", sc: "side:1" }),
      { type: "side_chat_conclusion_draft", sc: "side:1", text: "Approving this." },
    );
    const confirmed = reduce(drafted, {
      type: "side_chat_confirm_sent",
      sc: "side:1",
      anchor: 5,
    });
    expect(confirmed.sideChats["side:1"].confirming).toBe(true);
    expect(confirmed.awaitingConclusions).toEqual({ 5: "side:1" });

    const closed = reduce(confirmed, { type: "side_chat_closed", sc: "side:1" });
    expect(closed.sideChats["side:1"]).toBeUndefined();
    expect(closed.sideChatRefs).toEqual([]);
    // awaitingConclusions is only cleared by the matching `msg` arriving (see
    // the conclusion-chip describe block below) — closing the side chat alone
    // must not drop it, or a slow-arriving owner reply would go untagged.
    expect(closed.awaitingConclusions).toEqual({ 5: "side:1" });
  });

  it("discard_side_chat sent → side_chat_closed drops the record (no conclusion, item stays open)", () => {
    const discarding = reduce(openedSideChat(), {
      type: "side_chat_discard_sent",
      sc: "side:1",
    });
    expect(discarding.sideChats["side:1"].discarding).toBe(true);
    const closed = reduce(discarding, { type: "side_chat_closed", sc: "side:1" });
    expect(closed.sideChats["side:1"]).toBeUndefined();
    expect(closed.sideChatRefs).toEqual([]);
  });

  it("a host-initiated close (no confirm/discard in flight) marks the record ended, not deleted", () => {
    const s = openedSideChat();
    const closed = reduce(s, { type: "side_chat_closed", sc: "side:1" });
    expect(closed.sideChats["side:1"]).toMatchObject({ ended: true });
    // Dropped from the cheap "in progress" ref list even though the hydrated
    // record is kept (an open sheet needs it to render the terminal state).
    expect(closed.sideChatRefs).toEqual([]);
  });

  it("side_chat_closed for an unknown sc is a no-op", () => {
    const state = reduce(initialState(), { type: "side_chat_closed", sc: "side:ghost" });
    expect(state).toEqual(initialState());
  });
});

describe("item archived mid-side-chat (critique edge case)", () => {
  it("flags itemArchived on any live side chat for that item without touching others", () => {
    const withTwo = [
      { sc: "side:1", itemId: 5 },
      { sc: "side:2", itemId: 9 },
    ].reduce(
      (s, { sc, itemId }) => reduce(s, { type: "side_chat_open", sc, itemId, messages: [] }),
      initialState(),
    );

    const archived = reduce(withTwo, {
      type: "inbox_upsert",
      payload: { type: "inbox_upsert", item: inboxItem({ id: 5, status: "archived" }) },
    });

    expect(archived.sideChats["side:1"].itemArchived).toBe(true);
    expect(archived.sideChats["side:2"].itemArchived).toBe(false);
    // Conclude/Discard must remain reachable — nothing else about the record changes.
    expect(archived.sideChats["side:1"]).toMatchObject({ ended: false, confirming: false });
  });

  it("is idempotent and does not thrash object identity once already flagged", () => {
    const opened = reduce(initialState(), {
      type: "side_chat_open",
      sc: "side:1",
      itemId: 5,
      messages: [],
    });
    const once = reduce(opened, {
      type: "inbox_upsert",
      payload: { type: "inbox_upsert", item: inboxItem({ id: 5, status: "archived" }) },
    });
    const twice = reduce(once, {
      type: "inbox_upsert",
      payload: { type: "inbox_upsert", item: inboxItem({ id: 5, status: "archived" }) },
    });
    expect(twice.sideChats["side:1"]).toBe(once.sideChats["side:1"]);
  });
});

describe("conclusion chip memory (client-derived provenance)", () => {
  it("tags the owner reply landing in main chat with the matching anchor, and fires the one-shot land signal", () => {
    const drafted = reduce(
      reduce(
        reduce(initialState(), { type: "side_chat_open", sc: "side:1", itemId: 5, messages: [] }),
        { type: "side_chat_conclude_requested", sc: "side:1" },
      ),
      { type: "side_chat_conclusion_draft", sc: "side:1", text: "Approving this." },
    );
    const confirmed = reduce(drafted, {
      type: "side_chat_confirm_sent",
      sc: "side:1",
      anchor: 5,
    });

    // The host posts the owner's anchor-refed reply as a normal main `msg`.
    const landed = reduce(confirmed, {
      type: "msg",
      payload: { type: "msg", message: msg(200, "owner", "Approving this.", 5) },
    });

    expect(landed.conclusionChips).toEqual([200]);
    expect(landed.lastConclusion).toEqual({ sc: "side:1", messageId: 200 });
    expect(landed.awaitingConclusions).toEqual({});
  });

  it("does not tag an unrelated owner reply to the same anchor once already matched", () => {
    const confirmed = reduce(
      reduce(initialState(), { type: "side_chat_open", sc: "side:1", itemId: 5, messages: [] }),
      { type: "side_chat_confirm_sent", sc: "side:1", anchor: 5 },
    );
    const landed = reduce(confirmed, {
      type: "msg",
      payload: { type: "msg", message: msg(200, "owner", "the conclusion", 5) },
    });
    const second = reduce(landed, {
      type: "msg",
      payload: { type: "msg", message: msg(201, "owner", "a later, unrelated reply", 5) },
    });
    expect(second.conclusionChips).toEqual([200]);
    expect(second.lastConclusion).toEqual({ sc: "side:1", messageId: 200 }); // unchanged
  });

  it("a plain reply to an anchor with no pending conclusion is never tagged", () => {
    const state = reduce(initialState(), {
      type: "msg",
      payload: { type: "msg", message: msg(1, "owner", "just a normal reply", 5) },
    });
    expect(state.conclusionChips).toEqual([]);
    expect(state.lastConclusion).toBeNull();
  });

  it("clear_last_conclusion consumes the one-shot signal", () => {
    const confirmed = reduce(
      reduce(initialState(), { type: "side_chat_open", sc: "side:1", itemId: 5, messages: [] }),
      { type: "side_chat_confirm_sent", sc: "side:1", anchor: 5 },
    );
    const landed = reduce(confirmed, {
      type: "msg",
      payload: { type: "msg", message: msg(200, "owner", "the conclusion", 5) },
    });
    const cleared = reduce(landed, { type: "clear_last_conclusion" });
    expect(cleared.lastConclusion).toBeNull();
    expect(cleared.conclusionChips).toEqual([200]); // the chip itself persists
  });
});

describe("hello_ok reconciliation of live side chats", () => {
  it("seeds sideChatRefs from hello_ok.side_chats", () => {
    const state = reduce(initialState(), {
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: 0,
        messages: [],
        inbox: [],
        side_chats: [{ sc: "side:1", item_id: 5 }],
      },
    });
    expect(state.sideChatRefs).toEqual([{ sc: "side:1", item_id: 5 }]);
  });

  it("defaults to [] when hello_ok omits side_chats (pre-v2.0 host / nothing live)", () => {
    const state = reduce(initialState(), {
      type: "hello_ok",
      payload: { type: "hello_ok", latest_msg_id: 0, messages: [], inbox: [] },
    });
    expect(state.sideChatRefs).toEqual([]);
    expect(state.sideChats).toEqual({});
  });

  it("keeps a hydrated side chat still listed as live untouched", () => {
    const opened = reduce(initialState(), {
      type: "side_chat_open",
      sc: "side:1",
      itemId: 5,
      messages: [msg(1, "owner", "hi")],
    });
    const resynced = reduce(opened, {
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: 1,
        messages: [],
        inbox: [],
        side_chats: [{ sc: "side:1", item_id: 5 }],
      },
    });
    expect(resynced.sideChats["side:1"]).toBe(opened.sideChats["side:1"]);
  });

  it("drops a hydrated side chat that resolved (confirming/discarding) while offline", () => {
    const confirming = reduce(
      reduce(initialState(), { type: "side_chat_open", sc: "side:1", itemId: 5, messages: [] }),
      { type: "side_chat_confirm_sent", sc: "side:1", anchor: 5 },
    );
    const resynced = reduce(confirming, {
      type: "hello_ok",
      payload: { type: "hello_ok", latest_msg_id: 1, messages: [], inbox: [] },
    });
    expect(resynced.sideChats["side:1"]).toBeUndefined();
  });

  it("marks a hydrated-but-not-self-closed side chat ended when it vanishes from hello_ok (TTL reap elsewhere)", () => {
    const opened = reduce(initialState(), {
      type: "side_chat_open",
      sc: "side:1",
      itemId: 5,
      messages: [msg(1, "owner", "hi")],
    });
    const resynced = reduce(opened, {
      type: "hello_ok",
      payload: { type: "hello_ok", latest_msg_id: 1, messages: [], inbox: [] },
    });
    expect(resynced.sideChats["side:1"]).toMatchObject({ ended: true });
    // Its transcript is preserved so the terminal sheet can still show it.
    expect(resynced.sideChats["side:1"].messages).toEqual([msg(1, "owner", "hi")]);
  });
});
