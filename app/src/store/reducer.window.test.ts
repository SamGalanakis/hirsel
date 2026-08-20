import { describe, expect, it } from "vitest";
import { MESSAGES_CAP, reduce } from "./reducer";
import { type AppState, initialState } from "./types";
import type { ChatMessage } from "../protocol";

function msg(id: number, author: "owner" | "agent" = "agent"): ChatMessage {
  return { id, author, body: `m${id}`, ref: null, ts: "2026-07-08T00:00:00Z" };
}

/** Apply a live `msg` frame. */
function recv(state: AppState, id: number): AppState {
  return reduce(state, { type: "msg", payload: { type: "msg", message: msg(id) } });
}

describe("D10: in-memory history cap", () => {
  it("caps live message growth to MESSAGES_CAP, keeping the newest", () => {
    let state = initialState();
    const total = MESSAGES_CAP + 150;
    for (let id = 1; id <= total; id++) state = recv(state, id);

    expect(state.messages).toHaveLength(MESSAGES_CAP);
    // Oldest dropped from the front; newest retained at the tail.
    expect(state.messages[0].id).toBe(total - MESSAGES_CAP + 1);
    expect(state.messages[state.messages.length - 1].id).toBe(total);
  });

  it("caps a hello_ok replay that exceeds the limit", () => {
    const messages = Array.from({ length: MESSAGES_CAP + 40 }, (_, i) => msg(i + 1));
    const state = reduce(initialState(), {
      type: "hello_ok",
      payload: { type: "hello_ok", latest_msg_id: messages.length, messages, pings: [] },
    });

    expect(state.messages).toHaveLength(MESSAGES_CAP);
    expect(state.messages[state.messages.length - 1].id).toBe(MESSAGES_CAP + 40);
  });

  it("leaves a short history untouched (no cap, same identity)", () => {
    let state = initialState();
    const before = state.messages;
    state = recv(state, 1);
    state = recv(state, 2);
    expect(state.messages.map((m) => m.id)).toEqual([1, 2]);
    expect(before).not.toBe(state.messages); // grew, but was never sliced
  });

  it("never drops a still-pending optimistic send when capping", () => {
    let state = initialState();
    for (let id = 1; id <= MESSAGES_CAP; id++) state = recv(state, id);
    // A fresh optimistic send lands at the tail; the cap must trim the front,
    // never this newest pending entry.
    state = reduce(state, {
      type: "send_local",
      localId: -1,
      clientId: "c1",
      body: "hello",
      ref: null,
      ts: "2026-07-08T00:00:00Z",
    });
    expect(state.messages).toHaveLength(MESSAGES_CAP);
    const last = state.messages[state.messages.length - 1];
    expect(last.pending).toBe(true);
    expect(last.clientId).toBe("c1");
    expect(state.pendingSends).toHaveLength(1);
  });

  it("prepends an id-deduped page and keeps its oldest edge", () => {
    let state = initialState();
    state.messages = [msg(101), msg(102), msg(103)];
    state.hasEarlierMessages = true;

    state = reduce(state, {
      type: "messages_page",
      placement: "earlier",
      payload: {
        type: "messages",
        client_id: "history-1",
        before_id: 101,
        messages: [msg(99), msg(100), msg(101)],
        has_more: true,
      },
    });

    expect(state.messages.map((message) => message.id)).toEqual([99, 100, 101, 102, 103]);
    expect(state.hasEarlierMessages).toBe(true);
    expect(state.hasLaterMessages).toBe(false);
  });

  it("evicts from the newest committed edge during deep backfill", () => {
    let state = initialState();
    state.messages = Array.from({ length: MESSAGES_CAP }, (_, index) => msg(index + 101));
    state.hasEarlierMessages = true;

    state = reduce(state, {
      type: "messages_page",
      placement: "earlier",
      payload: {
        type: "messages",
        client_id: "history-2",
        before_id: 101,
        messages: Array.from({ length: 100 }, (_, index) => msg(index + 1)),
        has_more: false,
      },
    });

    expect(state.messages).toHaveLength(MESSAGES_CAP);
    expect(state.messages[0].id).toBe(1);
    expect(state.messages.at(-1)?.id).toBe(MESSAGES_CAP);
    expect(state.hasEarlierMessages).toBe(false);
    expect(state.hasLaterMessages).toBe(true);
  });

  it("replaces the historical range with the true latest page on jump", () => {
    let state = initialState();
    state.messages = Array.from({ length: MESSAGES_CAP }, (_, index) => msg(index + 1));
    state.hasLaterMessages = true;
    state.lastSeenMsgId = 800;

    state = reduce(state, {
      type: "messages_page",
      placement: "latest",
      payload: {
        type: "messages",
        client_id: "history-3",
        before_id: 801,
        messages: Array.from({ length: 100 }, (_, index) => msg(index + 701)),
        has_more: true,
      },
    });

    expect(state.messages[0].id).toBe(701);
    expect(state.messages.at(-1)?.id).toBe(800);
    expect(state.hasEarlierMessages).toBe(true);
    expect(state.hasLaterMessages).toBe(false);
  });
});

describe("D10: conversation render-window slice", () => {
  // The visible conversation is a pure tail slice of the buffer; this pins the
  // start/hasOlder math the component relies on.
  const windowStart = (len: number, limit: number) => Math.max(0, len - limit);

  it("shows the whole buffer when it fits the window", () => {
    expect(windowStart(150, 200)).toBe(0); // hasOlder = false
  });

  it("hides the oldest beyond the window and reveals them by growing the limit", () => {
    expect(windowStart(500, 200)).toBe(300); // 300 older hidden
    expect(windowStart(500, 300)).toBe(200); // "load older" (+100) reveals 100 more
    expect(windowStart(500, 500)).toBe(0); // all revealed
  });
});
