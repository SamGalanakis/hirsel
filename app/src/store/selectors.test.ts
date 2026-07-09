import { describe, expect, it } from "vitest";
import {
  isPingRead,
  isResolvedStatus,
  latestReplyForAnchor,
  openUnreadCount,
  partitionProcesses,
  runningProcessCount,
} from "./selectors";
import type { Ping, ProcessInfo } from "../protocol";
import type { DisplayMessage } from "./types";

function proc(overrides: Partial<ProcessInfo> = {}): ProcessInfo {
  return {
    id: "proc-1",
    kind: "subagent",
    label: "task",
    agent: null,
    model: null,
    state: "running",
    started_ts: "2026-07-09T00:00:00Z",
    last_event_ts: "2026-07-09T00:00:00Z",
    summary: null,
    ...overrides,
  };
}

function ownerMsg(overrides: Partial<DisplayMessage> = {}): DisplayMessage {
  return {
    id: 1,
    author: "owner",
    body: "hi",
    ref: null,
    ts: "2026-07-08T00:00:00Z",
    ...overrides,
  };
}

function item(overrides: Partial<Ping> = {}): Ping {
  return {
    id: 1,
    name: "test-ping",
    description: "Test Ping",
    content: "",
    anchor: 1,
    requires_response: false,
    quick_replies: [],
    status: "open",
    ts: "",
    ...overrides,
  };
}

describe("isPingRead (effective read = wire read minus local unread override)", () => {
  it("is false when the wire read flag is absent/false", () => {
    expect(isPingRead(item({ read: false }), [])).toBe(false);
    expect(isPingRead(item({ read: undefined }), [])).toBe(false);
  });

  it("is true when read=true and no override", () => {
    expect(isPingRead(item({ id: 7, read: true }), [])).toBe(true);
  });

  it("is false when read=true but the id is in the unread override set", () => {
    expect(isPingRead(item({ id: 7, read: true }), [7])).toBe(false);
  });
});

describe("openUnreadCount (email-like badge)", () => {
  it("counts open items that are not effectively read, regardless of requires_response", () => {
    expect(
      openUnreadCount(
        [
          item({ id: 1, requires_response: true, status: "open", read: false }),
          item({ id: 2, requires_response: false, status: "open", read: false }),
          item({ id: 3, requires_response: true, status: "open", read: true }), // read → excluded
          item({ id: 4, requires_response: true, status: "done", read: false }), // deleted → excluded
        ],
        [],
      ),
    ).toBe(2);
  });

  it("counts a read item as unread again when it has a local unread override", () => {
    const inbox = [item({ id: 1, status: "open", read: true })];
    expect(openUnreadCount(inbox, [])).toBe(0);
    expect(openUnreadCount(inbox, [1])).toBe(1);
  });
});

describe("latestReplyForAnchor (Inbox replied-state derivation)", () => {
  it("returns null when no owner message refs the anchor", () => {
    const messages: DisplayMessage[] = [
      ownerMsg({ id: 1, ref: null }),
      ownerMsg({ id: 2, author: "agent", ref: null }),
    ];
    expect(latestReplyForAnchor(messages, 5)).toBeNull();
  });

  it("derives a pending reply from an optimistic anchor-refed owner send", () => {
    const messages: DisplayMessage[] = [
      ownerMsg({ id: -1, body: "ship it", ref: 5, pending: true }),
    ];
    expect(latestReplyForAnchor(messages, 5)).toEqual({
      body: "ship it",
      pending: true,
      failed: false,
    });
  });

  it("flips pending → echoed (✓) once the send reconciles to a host message", () => {
    // After reconcile the pending flag is gone; the reply is a plain host msg.
    const messages: DisplayMessage[] = [ownerMsg({ id: 42, body: "ship it", ref: 5 })];
    const reply = latestReplyForAnchor(messages, 5);
    expect(reply?.pending).toBe(false);
    expect(reply?.failed).toBe(false);
  });

  it("surfaces a failed anchor-refed send", () => {
    const messages: DisplayMessage[] = [
      ownerMsg({ id: -1, body: "ship it", ref: 5, pending: true, failed: true }),
    ];
    expect(latestReplyForAnchor(messages, 5)).toMatchObject({ pending: true, failed: true });
  });

  it("does not treat an agent message that refs the anchor as the owner's reply", () => {
    const messages: DisplayMessage[] = [
      ownerMsg({ id: 10, author: "agent", body: "Got it", ref: 5 }),
    ];
    expect(latestReplyForAnchor(messages, 5)).toBeNull();
  });

  it("picks the most recent reply when the owner answers the same item twice", () => {
    const messages: DisplayMessage[] = [
      ownerMsg({ id: 20, body: "approve", ref: 5 }),
      ownerMsg({ id: 21, body: "actually reject", ref: 5, pending: true }),
    ];
    expect(latestReplyForAnchor(messages, 5)).toMatchObject({
      body: "actually reject",
      pending: true,
    });
  });

  it("scopes replies to the matching anchor only", () => {
    const messages: DisplayMessage[] = [
      ownerMsg({ id: 30, body: "for item A", ref: 5 }),
      ownerMsg({ id: 31, body: "for item B", ref: 8 }),
    ];
    expect(latestReplyForAnchor(messages, 5)?.body).toBe("for item A");
    expect(latestReplyForAnchor(messages, 8)?.body).toBe("for item B");
  });
});

describe("runningProcessCount (Processes tab badge)", () => {
  it("counts only running processes, ignoring every terminal state", () => {
    expect(
      runningProcessCount([
        proc({ id: "a", state: "running" }),
        proc({ id: "b", state: "running" }),
        proc({ id: "c", state: "done" }),
        proc({ id: "d", state: "failed" }),
        proc({ id: "e", state: "cancelled" }),
        proc({ id: "f", state: "abandoned" }),
      ]),
    ).toBe(2);
  });

  it("is 0 for an empty list", () => {
    expect(runningProcessCount([])).toBe(0);
  });
});

describe("partitionProcesses (Running/Finished, newest activity first)", () => {
  it("splits on running vs terminal and sorts each by last_event_ts desc", () => {
    const { running, finished } = partitionProcesses([
      proc({ id: "r-old", state: "running", last_event_ts: "2026-07-09T00:00:01Z" }),
      proc({ id: "r-new", state: "running", last_event_ts: "2026-07-09T00:00:09Z" }),
      proc({ id: "f-old", state: "done", last_event_ts: "2026-07-09T00:00:02Z" }),
      proc({ id: "f-new", state: "failed", last_event_ts: "2026-07-09T00:00:08Z" }),
    ]);
    expect(running.map((p) => p.id)).toEqual(["r-new", "r-old"]);
    expect(finished.map((p) => p.id)).toEqual(["f-new", "f-old"]);
  });
});

describe("isResolvedStatus (ADR-0009: done, or legacy archived, are both resolved)", () => {
  it("treats done and archived as resolved, open as not", () => {
    expect(isResolvedStatus("open")).toBe(false);
    expect(isResolvedStatus("done")).toBe(true);
    expect(isResolvedStatus("archived")).toBe(true);
  });
});
