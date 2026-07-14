import { fireEvent, render } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";

// Each test runs against a pristine copy of the store singleton: resetModules +
// dynamic import means the freshly-imported component and the store `dispatch`
// we drive it with share the same new module instance.
beforeEach(() => {
  vi.resetModules();
});

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
