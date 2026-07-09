import { describe, expect, it } from "vitest";
import { latestReplyForAnchor, openRequiresResponseCount } from "./selectors";
import type { DisplayMessage } from "./types";

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

describe("openRequiresResponseCount", () => {
  it("counts only open requires_response items", () => {
    expect(
      openRequiresResponseCount([
        { id: 1, content: "", anchor: 1, requires_response: true, quick_replies: [], status: "open", ts: "" },
        { id: 2, content: "", anchor: 2, requires_response: false, quick_replies: [], status: "open", ts: "" },
        { id: 3, content: "", anchor: 3, requires_response: true, quick_replies: [], status: "archived", ts: "" },
      ]),
    ).toBe(1);
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
