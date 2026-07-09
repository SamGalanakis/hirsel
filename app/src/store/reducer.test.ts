import { describe, expect, it } from "vitest";
import { reduce } from "./reducer";
import { initialState } from "./types";
import type { ChatMessage, Ping } from "../protocol";

function msg(id: number, author: "owner" | "agent", body: string, ref: number | null = null): ChatMessage {
  return { id, author, body, ref, ts: `2026-07-08T00:00:0${id}Z` };
}

function inboxItem(overrides: Partial<Ping> = {}): Ping {
  return {
    id: 1,
    name: "test-ping",
    description: "Test Ping",
    content: "hello",
    anchor: 1,
    requires_response: true,
    quick_replies: [],
    status: "open",
    ts: "2026-07-08T00:00:00Z",
    ...overrides,
  };
}

describe("hello_ok replay merge", () => {
  it("seeds messages, inbox, and lastSeenMsgId from a fresh connect", () => {
    const state = reduce(initialState(), {
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: 2,
        messages: [msg(1, "owner", "hi"), msg(2, "agent", "hello back")],
        pings: [inboxItem()],
      },
    });

    expect(state.messages).toHaveLength(2);
    expect(state.messages.map((m) => m.id)).toEqual([1, 2]);
    expect(state.pings).toHaveLength(1);
    expect(state.lastSeenMsgId).toBe(2);
  });

  it("merges replay with existing history without duplicating known messages", () => {
    const seeded = reduce(initialState(), {
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: 1,
        messages: [msg(1, "owner", "hi")],
        pings: [],
      },
    });

    const reconnected = reduce(seeded, {
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: 2,
        messages: [msg(2, "agent", "second")],
        pings: [],
      },
    });

    expect(reconnected.messages.map((m) => m.id)).toEqual([1, 2]);
  });

  it("reconciles a pending send against a replayed owner message (echo lost to disconnect)", () => {
    // Owner sent while connected; the frame reached the host but the `msg`
    // echo was lost to the disconnect. The reconnect replay contains the
    // real message - the pending bubble must clear, not duplicate.
    const withPending = reduce(initialState(), {
      type: "send_local",
      localId: -1,
      clientId: "c1",
      body: "made it to the host",
      ref: null,
      ts: "2026-07-08T00:00:00Z",
    });

    const afterReconnect = reduce(withPending, {
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: 7,
        messages: [msg(7, "owner", "made it to the host")],
        pings: [],
      },
    });

    expect(afterReconnect.messages).toHaveLength(1);
    expect(afterReconnect.messages[0]).toEqual(msg(7, "owner", "made it to the host"));
    expect(afterReconnect.messages[0].pending).toBeUndefined();
    expect(afterReconnect.pendingSends).toEqual([]);
  });

  it("keeps non-matching pending sends through a replay so they are still resent", () => {
    const s1 = reduce(initialState(), {
      type: "send_local",
      localId: -1,
      clientId: "c1",
      body: "reached the host",
      ref: null,
      ts: "2026-07-08T00:00:00Z",
    });
    const s2 = reduce(s1, {
      type: "send_local",
      localId: -2,
      clientId: "c2",
      body: "never reached the host",
      ref: null,
      ts: "2026-07-08T00:00:01Z",
    });

    const afterReconnect = reduce(s2, {
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: 7,
        messages: [msg(7, "owner", "reached the host")],
        pings: [],
      },
    });

    // c1 reconciled away; c2 survives as pending and stays in pendingSends
    // for the post-hello_ok resend.
    expect(afterReconnect.messages).toHaveLength(2);
    expect(afterReconnect.messages[0]).toEqual(msg(7, "owner", "reached the host"));
    expect(afterReconnect.messages[1]).toMatchObject({
      pending: true,
      clientId: "c2",
      body: "never reached the host",
    });
    expect(afterReconnect.pendingSends).toEqual([
      { clientId: "c2", body: "never reached the host", ref: null },
    ]);
  });

  it("does not re-reconcile an already-known replayed message against new pending sends", () => {
    // A message already merged in a previous replay must not consume a new
    // pending entry that happens to share its body.
    const seeded = reduce(initialState(), {
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: 1,
        messages: [msg(1, "owner", "same words")],
        pings: [],
      },
    });
    const withPending = reduce(seeded, {
      type: "send_local",
      localId: -1,
      clientId: "c1",
      body: "same words",
      ref: null,
      ts: "2026-07-08T00:00:02Z",
    });

    const afterReconnect = reduce(withPending, {
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: 1,
        messages: [msg(1, "owner", "same words")],
        pings: [],
      },
    });

    expect(afterReconnect.messages).toHaveLength(2);
    expect(afterReconnect.pendingSends).toEqual([
      { clientId: "c1", body: "same words", ref: null },
    ]);
  });

  it("keeps still-pending optimistic sends after the tail of a replay", () => {
    const withPending = reduce(initialState(), {
      type: "send_local",
      localId: -1,
      clientId: "c1",
      body: "queued while offline",
      ref: null,
      ts: "2026-07-08T00:00:00Z",
    });

    const afterReconnect = reduce(withPending, {
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: 0,
        messages: [],
        pings: [],
      },
    });

    expect(afterReconnect.messages).toHaveLength(1);
    expect(afterReconnect.messages[0].pending).toBe(true);
    expect(afterReconnect.pendingSends).toHaveLength(1);
  });
});

describe("msg append", () => {
  it("appends an agent message", () => {
    const state = reduce(initialState(), {
      type: "msg",
      payload: { type: "msg", message: msg(1, "agent", "hi there") },
    });
    expect(state.messages).toEqual([msg(1, "agent", "hi there")]);
    expect(state.lastSeenMsgId).toBe(1);
  });

  it("does not duplicate an already-known message id", () => {
    const once = reduce(initialState(), {
      type: "msg",
      payload: { type: "msg", message: msg(1, "agent", "hi") },
    });
    const twice = reduce(once, {
      type: "msg",
      payload: { type: "msg", message: msg(1, "agent", "hi") },
    });
    expect(twice.messages).toHaveLength(1);
  });

  it("tracks the max seen id across out-of-order deliveries", () => {
    const state = reduce(
      reduce(initialState(), {
        type: "msg",
        payload: { type: "msg", message: msg(5, "agent", "five") },
      }),
      { type: "msg", payload: { type: "msg", message: msg(3, "agent", "three") } },
    );
    expect(state.lastSeenMsgId).toBe(5);
  });
});

describe("inbox upsert transitions", () => {
  it("inserts a new item", () => {
    const state = reduce(initialState(), {
      type: "ping_upsert",
      payload: { type: "ping_upsert", ping: inboxItem({ id: 1 }) },
    });
    expect(state.pings).toHaveLength(1);
  });

  it("replaces an existing item by id rather than duplicating it", () => {
    const opened = reduce(initialState(), {
      type: "ping_upsert",
      payload: { type: "ping_upsert", ping: inboxItem({ id: 1, status: "open" }) },
    });
    const archived = reduce(opened, {
      type: "ping_upsert",
      payload: { type: "ping_upsert", ping: inboxItem({ id: 1, status: "done" }) },
    });
    expect(archived.pings).toHaveLength(1);
    expect(archived.pings[0].status).toBe("done");
  });
});

describe("v1.3 read state", () => {
  it("ping_upsert carries the wire read flag through", () => {
    const state = reduce(initialState(), {
      type: "ping_upsert",
      payload: { type: "ping_upsert", ping: inboxItem({ id: 1, read: true }) },
    });
    expect(state.pings[0].read).toBe(true);
  });

  it("read_local optimistically flips read=true on the item", () => {
    const seeded = reduce(initialState(), {
      type: "ping_upsert",
      payload: { type: "ping_upsert", ping: inboxItem({ id: 1, read: false }) },
    });
    const read = reduce(seeded, { type: "read_local", pingId: 1 });
    expect(read.pings[0].read).toBe(true);
  });

  it("read_local clears a prior manual unread override (reading wins)", () => {
    const seeded = reduce(initialState(), {
      type: "ping_upsert",
      payload: { type: "ping_upsert", ping: inboxItem({ id: 1, read: true }) },
    });
    const unread = reduce(seeded, { type: "mark_unread_local", pingId: 1 });
    expect(unread.unreadOverrides).toEqual([1]);
    const reread = reduce(unread, { type: "read_local", pingId: 1 });
    expect(reread.unreadOverrides).toEqual([]);
    expect(reread.pings[0].read).toBe(true);
  });

  it("mark_unread_local records a client-only override without touching the wire read flag", () => {
    const seeded = reduce(initialState(), {
      type: "ping_upsert",
      payload: { type: "ping_upsert", ping: inboxItem({ id: 1, read: true }) },
    });
    const unread = reduce(seeded, { type: "mark_unread_local", pingId: 1 });
    // Wire flag is untouched (there is no unread op); only the override records it.
    expect(unread.pings[0].read).toBe(true);
    expect(unread.unreadOverrides).toEqual([1]);
  });

  it("mark_unread_local is idempotent (no duplicate ids)", () => {
    const s1 = reduce(initialState(), { type: "mark_unread_local", pingId: 5 });
    const s2 = reduce(s1, { type: "mark_unread_local", pingId: 5 });
    expect(s2.unreadOverrides).toEqual([5]);
  });
});

describe("optimistic-send reconciliation", () => {
  it("renders a send_local optimistically as a pending owner message", () => {
    const state = reduce(initialState(), {
      type: "send_local",
      localId: -2,
      clientId: "c1",
      body: "hello agent",
      ref: null,
      ts: "2026-07-08T00:00:00Z",
    });
    expect(state.messages).toHaveLength(1);
    expect(state.messages[0]).toMatchObject({
      author: "owner",
      body: "hello agent",
      pending: true,
      clientId: "c1",
    });
    expect(state.pendingSends).toEqual([{ clientId: "c1", body: "hello agent", ref: null }]);
  });

  it("replaces the oldest matching pending entry with the host-echoed msg", () => {
    const withPending = reduce(initialState(), {
      type: "send_local",
      localId: -3,
      clientId: "c1",
      body: "hello agent",
      ref: null,
      ts: "2026-07-08T00:00:00Z",
    });

    const reconciled = reduce(withPending, {
      type: "msg",
      payload: { type: "msg", message: msg(42, "owner", "hello agent") },
    });

    expect(reconciled.messages).toHaveLength(1);
    expect(reconciled.messages[0]).toEqual(msg(42, "owner", "hello agent"));
    expect(reconciled.pendingSends).toEqual([]);
  });

  it("reconciles FIFO when two pending sends share a body", () => {
    const s1 = reduce(initialState(), {
      type: "send_local",
      localId: -4,
      clientId: "c1",
      body: "same text",
      ref: null,
      ts: "2026-07-08T00:00:00Z",
    });
    const s2 = reduce(s1, {
      type: "send_local",
      localId: -5,
      clientId: "c2",
      body: "same text",
      ref: null,
      ts: "2026-07-08T00:00:01Z",
    });

    const afterFirstAck = reduce(s2, {
      type: "msg",
      payload: { type: "msg", message: msg(10, "owner", "same text") },
    });

    expect(afterFirstAck.pendingSends).toEqual([{ clientId: "c2", body: "same text", ref: null }]);
    expect(afterFirstAck.messages.filter((m) => m.pending)).toHaveLength(1);
    expect(afterFirstAck.messages.find((m) => m.id === 10)).toBeDefined();
  });

  it("appends without reconciling when no pending send matches", () => {
    const state = reduce(initialState(), {
      type: "msg",
      payload: { type: "msg", message: msg(1, "owner", "typed on another device") },
    });
    expect(state.messages).toHaveLength(1);
    expect(state.messages[0].pending).toBeUndefined();
  });
});

describe("ADR-0009: replying resolves an Inbox Item to done", () => {
  function withOpenItem() {
    return reduce(initialState(), {
      type: "ping_upsert",
      payload: { type: "ping_upsert", ping: inboxItem({ id: 1, anchor: 5, status: "open" }) },
    });
  }

  it("optimistically flips an open item to done when a send_local anchors to it", () => {
    const next = reduce(withOpenItem(), {
      type: "send_local",
      localId: -1,
      clientId: "c1",
      body: "approved",
      ref: 5,
      ts: "2026-07-08T00:01:00Z",
    });
    expect(next.pings[0].status).toBe("done");
  });

  it("leaves the item open when the send anchors elsewhere (or nowhere)", () => {
    const other = reduce(withOpenItem(), {
      type: "send_local",
      localId: -1,
      clientId: "c1",
      body: "unrelated",
      ref: 99,
      ts: "2026-07-08T00:01:00Z",
    });
    expect(other.pings[0].status).toBe("open");
    const plain = reduce(withOpenItem(), {
      type: "send_local",
      localId: -2,
      clientId: "c2",
      body: "hello",
      ref: null,
      ts: "2026-07-08T00:01:00Z",
    });
    expect(plain.pings[0].status).toBe("open");
  });

  it("resolves the item on a Side Chat conclusion confirm (no send_local of its own)", () => {
    const opened = reduce(withOpenItem(), {
      type: "side_chat_open",
      sc: "side:1",
      pingId: 1,
      messages: [],
    });
    const confirmed = reduce(opened, { type: "side_chat_confirm_sent", sc: "side:1", anchor: 5 });
    expect(confirmed.pings[0].status).toBe("done");
  });
});
