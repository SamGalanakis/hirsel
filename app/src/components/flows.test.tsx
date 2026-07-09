import { fireEvent, render } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { InboxItem } from "../protocol";

// Each test runs against a pristine copy of the store singleton: resetModules +
// dynamic import means the freshly-imported component and the store `dispatch`
// we drive it with share the same new module instance.
beforeEach(() => {
  vi.resetModules();
});

function inboxItem(overrides: Partial<InboxItem> = {}): InboxItem {
  return {
    id: 1,
    content: "Approve the change?",
    anchor: 5,
    requires_response: true,
    quick_replies: [
      { value: "approve", label: "Approve" },
      { value: "reject", label: "Reject" },
    ],
    status: "open",
    ts: "2026-07-08T00:00:00Z",
    ...overrides,
  };
}

describe("Composer send", () => {
  it("dispatches send_local for the typed body via the ws client", async () => {
    const store = await import("../store/store");
    // The ChatView's send handler calls getClient()?.sendMessage; mock the ws
    // client so sendMessage performs the same send_local dispatch the real one
    // does, without needing a live WebSocket (absent in jsdom).
    vi.doMock("../ws/client", () => ({
      getClient: () => ({
        sendMessage: (body: string, ref: number | null) => {
          store.dispatch({
            type: "send_local",
            localId: -1,
            clientId: "test-client",
            body,
            ref,
            ts: "2026-07-08T00:00:00Z",
          });
          return -1;
        },
        archiveItem: () => {},
      }),
    }));

    const { ChatView } = await import("./chat/ChatView");
    const { getByPlaceholderText, getByLabelText } = render(() => <ChatView />);

    const textarea = getByPlaceholderText("Message the Agent…") as HTMLTextAreaElement;
    fireEvent.input(textarea, { target: { value: "hello agent" } });
    fireEvent.click(getByLabelText("Send"));

    expect(store.state.messages).toHaveLength(1);
    expect(store.state.messages[0]).toMatchObject({
      author: "owner",
      body: "hello agent",
      ref: null,
      pending: true,
    });
    expect(store.state.pendingSends).toEqual([
      { clientId: "test-client", body: "hello agent", ref: null },
    ]);
  });
});

describe("Inbox inline reply", () => {
  it("quick reply sends the tapped value anchored to the item without navigating", async () => {
    const store = await import("../store/store");
    const sendMessage = vi.fn((_body: string, _ref: number | null) => -9);
    vi.doMock("../ws/client", () => ({
      getClient: () => ({ sendMessage, archiveItem: vi.fn(), readItem: vi.fn() }),
    }));

    const { InboxView } = await import("./inbox/InboxView");
    store.dispatch({
      type: "inbox_upsert",
      payload: { type: "inbox_upsert", item: inboxItem({ id: 1, anchor: 5 }) },
    });

    const { getByText } = render(() => <InboxView />);
    fireEvent.click(getByText("Approve"));

    // Anchor-refed send: value = quick reply value, ref = item.anchor.
    expect(sendMessage).toHaveBeenCalledWith("approve", 5);
    // Answered in place: no navigation, no one-shot scroll request (the Tray
    // that hosts this view has no reason to collapse or scroll Chat either).
    expect(store.state.scrollToMessageId).toBeNull();
  });

  it("inline freeform input sends the typed body anchored to the item, in place", async () => {
    const store = await import("../store/store");
    const sendMessage = vi.fn((_body: string, _ref: number | null) => -7);
    vi.doMock("../ws/client", () => ({
      getClient: () => ({ sendMessage, archiveItem: vi.fn(), readItem: vi.fn() }),
    }));

    const { InboxView } = await import("./inbox/InboxView");
    // requires_response item exposes the inline input expanded by default.
    store.dispatch({
      type: "inbox_upsert",
      payload: { type: "inbox_upsert", item: inboxItem({ id: 1, anchor: 9 }) },
    });

    const { getByPlaceholderText, getByLabelText } = render(() => <InboxView />);
    const input = getByPlaceholderText("Reply…") as HTMLTextAreaElement;
    fireEvent.input(input, { target: { value: "ship it" } });
    fireEvent.click(getByLabelText("Send reply"));

    expect(sendMessage).toHaveBeenCalledWith("ship it", 9);
    // The input clears after sending so the card is ready for the next reply.
    expect(input.value).toBe("");
  });
});
