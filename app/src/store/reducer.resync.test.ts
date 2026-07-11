import { describe, expect, it } from "vitest";
import { reduce } from "./reducer";
import { initialState } from "./types";
import type { ChatMessage, Ping, ProcessInfo } from "../protocol";

function msg(id: number, author: "owner" | "agent", body: string): ChatMessage {
  return { id, author, body, ref: null, ts: "2026-07-08T00:00:00Z" };
}

function ping(id: number): Ping {
  return {
    id,
    name: `p${id}`,
    description: "d",
    content: "c",
    anchor: id,
    requires_response: true,
    quick_replies: [],
    status: "open",
    ts: "2026-07-08T00:00:00Z",
  };
}

function proc(id: string): ProcessInfo {
  return {
    id,
    kind: "subagent",
    label: id,
    agent: null,
    model: null,
    state: "running",
    started_ts: "2026-07-08T00:00:00Z",
    last_event_ts: "2026-07-08T00:00:00Z",
    summary: null,
  };
}

describe("C7: idempotent resync on a second hello_ok", () => {
  it("replaces state with the second full snapshot, no residue from the first", () => {
    const first = reduce(initialState(), {
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: 3,
        messages: [msg(1, "owner", "a"), msg(2, "agent", "b"), msg(3, "owner", "c")],
        pings: [ping(10), ping(11)],
        processes: [proc("p1")],
      },
    });
    // A client-only "mark unread" override on a ping that will vanish in the resync.
    const withOverride = reduce(first, { type: "mark_unread_local", pingId: 11 });
    expect(withOverride.unreadOverrides).toContain(11);

    // A full resync (host build_snapshot with no cursor → starts at id 1): during
    // the lag gap msg 2 was removed, ping 11 resolved, and msg 4 + ping 12 arrived.
    const second = reduce(withOverride, {
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: 4,
        messages: [msg(1, "owner", "a"), msg(3, "owner", "c"), msg(4, "agent", "d")],
        pings: [ping(10), ping(12)],
        processes: [proc("p2")],
      },
    });

    // messages: exactly the second snapshot — msg 2 dropped, msg 4 added, no dups.
    expect(second.messages.map((m) => m.id)).toEqual([1, 3, 4]);
    // pings + processes: clean replace, nothing stale lingers.
    expect(second.pings.map((p) => p.id)).toEqual([10, 12]);
    expect(second.processes.map((p) => p.id)).toEqual(["p2"]);
    // cursor recomputed from the new snapshot.
    expect(second.lastSeenMsgId).toBe(4);
    // unread override for the now-absent ping 11 is pruned (recomputed).
    expect(second.unreadOverrides).not.toContain(11);
    // ephemeral live timeline never survives a resync.
    expect(second.turnEvents).toEqual([]);
  });

  it("is fully idempotent when the same snapshot is delivered twice", () => {
    const payload = {
      type: "hello_ok" as const,
      latest_msg_id: 2,
      messages: [msg(1, "owner", "a"), msg(2, "agent", "b")],
      pings: [ping(10)],
      processes: [proc("p1")],
    };
    const once = reduce(initialState(), { type: "hello_ok", payload });
    const twice = reduce(once, { type: "hello_ok", payload });
    expect(twice.messages.map((m) => m.id)).toEqual([1, 2]);
    expect(twice.pings.map((p) => p.id)).toEqual([10]);
  });

  it("still preserves older local history on a cursor-based handshake replay", () => {
    const seeded = reduce(initialState(), {
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: 2,
        messages: [msg(1, "owner", "a"), msg(2, "agent", "b")],
        pings: [],
      },
    });
    // Reconnect: the host replays only ids > the client's last_seen (partial).
    const reconnected = reduce(seeded, {
      type: "hello_ok",
      payload: {
        type: "hello_ok",
        latest_msg_id: 3,
        messages: [msg(3, "owner", "c")],
        pings: [],
      },
    });
    // Older local history (1, 2) survives the partial replay; 3 is appended.
    expect(reconnected.messages.map((m) => m.id)).toEqual([1, 2, 3]);
  });
});
