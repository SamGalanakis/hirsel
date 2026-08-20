import { describe, expect, it } from "vitest";
import { reduce } from "./reducer";
import { initialState } from "./types";
import type { Blob, ChatMessage } from "../protocol";

function ownerMsg(id: number, body: string): ChatMessage {
  return { id, author: "owner", body, ref: null, ts: `2026-07-09T00:00:0${id}Z` };
}

function blob(id: string, name = "photo.png", mime = "image/png", size = 1234): Blob {
  return { id, name, mime, size };
}

function sendLocal(clientId: string, body: string, extra: Record<string, unknown> = {}) {
  return {
    type: "send_local" as const,
    localId: -1,
    clientId,
    body,
    ref: null,
    ts: "2026-07-09T00:00:00Z",
    ...extra,
  };
}

describe("send_local with attachments and mode", () => {
  it("carries attachments onto the optimistic message and pendingSend blob ids", () => {
    const s = reduce(initialState(), sendLocal("c1", "look", { attachments: [blob("b1")] }));
    expect(s.messages[0].attachments).toEqual([blob("b1")]);
    expect(s.pendingSends[0]).toEqual({
      clientId: "c1",
      body: "look",
      ref: null,
      attachments: ["b1"],
      mode: "send",
      mentions: [],
    });
  });

  it("spells a default send's empty attachments/mentions and \"send\" mode out in full", () => {
    const s = reduce(initialState(), sendLocal("c1", "hi"));
    expect(s.pendingSends[0]).toEqual({
      clientId: "c1",
      body: "hi",
      ref: null,
      attachments: [],
      mode: "send",
      mentions: [],
    });
    expect(s.messages[0].mode).toBe("send");
  });

  it("records a next_turn mode on the pendingSend", () => {
    const s = reduce(initialState(), sendLocal("c1", "later", { mode: "next_turn" }));
    expect(s.pendingSends[0]).toEqual({
      clientId: "c1",
      body: "later",
      ref: null,
      attachments: [],
      mode: "next_turn",
      mentions: [],
    });
  });
});

describe("reconciliation preserves cancellability for next_turn", () => {
  it("keeps clientId + mode on a reconciled next_turn message", () => {
    const s1 = reduce(initialState(), sendLocal("c1", "queued one", { mode: "next_turn" }));
    const s2 = reduce(s1, { type: "msg", payload: { type: "msg", message: ownerMsg(5, "queued one") } });
    expect(s2.messages).toHaveLength(1);
    expect(s2.messages[0]).toMatchObject({ id: 5, clientId: "c1", mode: "next_turn" });
    expect(s2.messages[0].pending).toBeUndefined();
  });

  it("drops clientId on a reconciled plain send (bare host message)", () => {
    const s1 = reduce(initialState(), sendLocal("c1", "plain one"));
    const s2 = reduce(s1, { type: "msg", payload: { type: "msg", message: ownerMsg(6, "plain one") } });
    expect(s2.messages[0]).toEqual(ownerMsg(6, "plain one"));
  });
});

describe("msg_removed tombstone", () => {
  it("drops the bubble with that id", () => {
    const seeded = reduce(initialState(), {
      type: "msg",
      payload: { type: "msg", message: ownerMsg(7, "cancel me") },
    });
    const after = reduce(seeded, { type: "msg_removed", id: 7 });
    expect(after.messages.find((m) => m.id === 7)).toBeUndefined();
  });

  it("also clears a still-pending optimistic entry + its pendingSend", () => {
    // A queued next_turn send, reconciled to id 8, then cancelled.
    const s1 = reduce(initialState(), sendLocal("c9", "queued", { mode: "next_turn" }));
    const s2 = reduce(s1, { type: "msg", payload: { type: "msg", message: ownerMsg(8, "queued") } });
    // The reconciled message keeps clientId c9; simulate the host tombstone.
    const after = reduce(s2, { type: "msg_removed", id: 8 });
    expect(after.messages).toHaveLength(0);
    expect(after.pendingSends).toHaveLength(0);
  });

  it("tombstones the id so a late echo cannot re-materialize the bubble", () => {
    // Out-of-order: cancel (msg_removed) is processed while the send is still an
    // optimistic negative-id entry, then the host echo arrives afterwards.
    const s1 = reduce(initialState(), sendLocal("cx", "cancel me", { mode: "next_turn" }));
    const removed = reduce(s1, { type: "msg_removed", id: 9 }); // id not present yet
    expect(removed.removedIds).toContain(9);
    // the optimistic entry is still there (id was negative, not 9) — but the echo…
    const echo = reduce(removed, { type: "msg", payload: { type: "msg", message: ownerMsg(9, "cancel me") } });
    expect(echo.messages.find((m) => m.id === 9)).toBeUndefined();
  });

  it("drops a tombstoned id from a hello_ok replay", () => {
    const tombstoned = reduce(initialState(), { type: "msg_removed", id: 4 });
    const after = reduce(tombstoned, {
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: 4,
        messages: [ownerMsg(4, "should stay gone")],
        pings: [],
      },
    });
    expect(after.messages.find((m) => m.id === 4)).toBeUndefined();
  });
});

describe("failed-send + retry", () => {
  it("marks a pending send failed then clears it on retry", () => {
    const s1 = reduce(initialState(), sendLocal("c1", "stuck"));
    const failed = reduce(s1, { type: "send_failed", clientId: "c1" });
    expect(failed.messages[0].failed).toBe(true);
    const retried = reduce(failed, { type: "send_retry", clientId: "c1" });
    expect(retried.messages[0].failed).toBe(false);
    expect(retried.messages[0].pending).toBe(true);
  });

  it("does not touch already-reconciled (non-pending) messages", () => {
    const s1 = reduce(initialState(), {
      type: "msg",
      payload: { type: "msg", message: ownerMsg(3, "already here") },
    });
    const after = reduce(s1, { type: "send_failed", clientId: "whatever" });
    expect(after.messages[0].failed).toBeUndefined();
  });
});
