import { fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { InboxItem } from "../../protocol";

// Component-level gates for the Side Chat sheet (ADR-0008): Discuss opens the
// sheet, the conclude flow reaches confirm_conclusion with the Owner's edited
// text, and discard requires its confirm. Same pristine-store-per-test
// pattern as flows.test.tsx / inbox-reply-scenario.test.tsx: resetModules +
// dynamic import so the freshly-imported store/components share one instance,
// and the ws client is mocked to synthesize the host's v2.0 responses via
// direct `dispatch` calls instead of a live socket.
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

/** Mounts ChatView (which owns both the Tray/InboxView Discuss entry point
 * and the SideChatSheet itself) with a fake ws client that behaves like the
 * mock server's side-chat handling, synchronously. */
async function setup(itemOverrides: Partial<InboxItem> = {}) {
  const store = await import("../../store/store");
  let scCounter = 0;
  const fakeClient = {
    openSideChat: vi.fn((itemId: number) => {
      const sc = `side:${++scCounter}`;
      store.dispatch({ type: "side_chat_open", sc, itemId, messages: [] });
    }),
    sendSideMessage: vi.fn((sc: string, body: string, ref: number | null) => {
      const localId = -1000 - scCounter;
      store.dispatch({
        type: "side_chat_send_local",
        sc,
        localId,
        clientId: `c${localId}`,
        body,
        ref,
        ts: "2026-07-08T00:01:00Z",
      });
      return localId;
    }),
    cancelSideTurn: vi.fn(),
    concludeSideChat: vi.fn((sc: string) => {
      store.dispatch({ type: "side_chat_conclude_requested", sc });
      store.dispatch({ type: "side_chat_conclusion_draft", sc, text: "Approving — looks good." });
    }),
    confirmConclusion: vi.fn(),
    discardSideChat: vi.fn((sc: string) => {
      store.dispatch({ type: "side_chat_closed", sc });
    }),
    archiveItem: vi.fn(),
    readItem: vi.fn(),
    sendMessage: vi.fn(() => -1),
  };
  vi.doMock("../../ws/client", () => ({ getClient: () => fakeClient }));

  const { ChatView } = await import("../chat/ChatView");
  // Conclude is gated on being connected (offline shows "Reconnect to
  // conclude" instead) — these tests aren't exercising that, so start online.
  store.dispatch({ type: "connection_status", status: "connected" });
  store.dispatch({
    type: "inbox_upsert",
    payload: { type: "inbox_upsert", item: inboxItem(itemOverrides) },
  });
  const screen = render(() => <ChatView />);

  // Expand the Tray to reach the item card's Discuss affordance.
  await fireEvent.click(screen.getByLabelText("Open inbox"));

  return { store, screen, fakeClient };
}

describe("Discuss opens the Side Chat sheet", () => {
  it("opens the sheet with the seed card, and hides it again on leave", async () => {
    const { screen, fakeClient } = await setup();

    const discussButton = screen.getByRole("button", { name: /Discuss/ });
    await fireEvent.click(discussButton);

    expect(fakeClient.openSideChat).toHaveBeenCalledWith(1);
    await waitFor(() =>
      expect(screen.getByText(/Forked from your chat/)).toBeTruthy(),
    );
    expect(screen.getByText(/Side chat ·/)).toBeTruthy();

    // Leave (header-left `‹ Chat`) hides the sheet without discarding/concluding.
    await fireEvent.click(screen.getByLabelText(/Leave side chat/));
    expect(screen.queryByText(/Forked from your chat/)).toBeNull();
  });

  it("flips to 'in progress · resume' once a side chat is live for the item", async () => {
    const { screen, fakeClient } = await setup();
    await fireEvent.click(screen.getByRole("button", { name: /Discuss/ }));
    await waitFor(() => expect(screen.getByText(/Side chat ·/)).toBeTruthy());
    await fireEvent.click(screen.getByLabelText(/Leave side chat/));

    expect(screen.getByRole("button", { name: /in progress · resume/ })).toBeTruthy();
    expect(screen.queryByRole("button", { name: /^Discuss$/ })).toBeNull();

    await fireEvent.click(screen.getByRole("button", { name: /in progress · resume/ }));
    // Resuming calls open_side_chat again (idempotent on the host) rather than
    // creating a second side chat.
    expect(fakeClient.openSideChat).toHaveBeenCalledTimes(2);
    await waitFor(() => expect(screen.getByText(/Side chat ·/)).toBeTruthy());
  });
});

describe("Conclude flow reaches confirm_conclusion with the Owner's edited text", () => {
  it("drafts, lets the Owner edit, and sends the edited text on 'Send reply'", async () => {
    const { screen, fakeClient } = await setup();
    await fireEvent.click(screen.getByRole("button", { name: /Discuss/ }));
    await waitFor(() => expect(screen.getByText(/Side chat ·/)).toBeTruthy());

    await fireEvent.click(screen.getByRole("button", { name: "Conclude" }));
    expect(fakeClient.concludeSideChat).toHaveBeenCalledTimes(1);

    // The draft opens the confirmation sheet, pre-filled.
    const textarea = await screen.findByLabelText("Reply text") as HTMLTextAreaElement;
    expect(textarea.value).toBe("Approving — looks good.");
    expect(screen.getByText(/Original question/)).toBeTruthy();

    // The Owner edits it before sending. Scope to the confirm dialog itself —
    // the Tray's own inline ReplyInput (still mounted behind the sheet, this
    // item being requires_response) has its own "Send reply" button.
    const confirmDialog = screen.getByRole("dialog", { name: "Send this reply?" });
    fireEvent.input(textarea, { target: { value: "Approving, ship it Friday." } });
    await fireEvent.click(within(confirmDialog).getByRole("button", { name: "Send reply" }));

    expect(fakeClient.confirmConclusion).toHaveBeenCalledWith(
      "side:1",
      "Approving, ship it Friday.",
      5,
    );
  });

  it("'Keep editing' returns to the side chat without discarding or sending", async () => {
    const { screen, fakeClient } = await setup();
    await fireEvent.click(screen.getByRole("button", { name: /Discuss/ }));
    await waitFor(() => expect(screen.getByText(/Side chat ·/)).toBeTruthy());
    await fireEvent.click(screen.getByRole("button", { name: "Conclude" }));
    await screen.findByLabelText("Reply text");

    await fireEvent.click(screen.getByRole("button", { name: "Keep editing" }));

    expect(screen.queryByLabelText("Reply text")).toBeNull();
    expect(fakeClient.confirmConclusion).not.toHaveBeenCalled();
    expect(fakeClient.discardSideChat).not.toHaveBeenCalled();
    // Still on the side chat, not booted back to Chat.
    expect(screen.getByText(/Side chat ·/)).toBeTruthy();
  });
});

describe("Discard requires its confirm", () => {
  // Each test opens the ⋯ menu exactly once and starts from a fresh render:
  // Kobalte's dropdown briefly holds a pointer-events lock on the rest of the
  // page during its close transition, and jsdom (no real CSS transitions)
  // doesn't reliably signal that lock has lifted — reusing one render across
  // two open/select cycles is flaky for that environment reason, not a
  // product one. The dialog's own buttons use fireEvent, which dispatches the
  // DOM event directly and isn't subject to that lock.

  it("Cancel leaves the side chat untouched", async () => {
    const { screen, fakeClient } = await setup();
    await fireEvent.click(screen.getByRole("button", { name: /Discuss/ }));
    await waitFor(() => expect(screen.getByText(/Side chat ·/)).toBeTruthy());

    const user = userEvent.setup();
    await user.click(screen.getByLabelText("Side chat actions"));
    await user.click(await within(document.body).findByRole("menuitem", { name: "Discard" }));

    // Not yet discarded — a confirm dialog stands in the way.
    expect(fakeClient.discardSideChat).not.toHaveBeenCalled();
    expect(screen.getByText(/Discard this side chat\?/)).toBeTruthy();
    expect(screen.getByText(/Your notes here are deleted; the item stays open\./)).toBeTruthy();

    await fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(fakeClient.discardSideChat).not.toHaveBeenCalled();
    expect(screen.getByText(/Side chat ·/)).toBeTruthy(); // still open
  });

  it("Discard (confirmed) closes the sheet with no conclusion; the item stays open", async () => {
    const { screen, fakeClient } = await setup();
    await fireEvent.click(screen.getByRole("button", { name: /Discuss/ }));
    await waitFor(() => expect(screen.getByText(/Side chat ·/)).toBeTruthy());

    const user = userEvent.setup();
    await user.click(screen.getByLabelText("Side chat actions"));
    await user.click(await within(document.body).findByRole("menuitem", { name: "Discard" }));
    await fireEvent.click(screen.getByRole("button", { name: "Discard" }));

    expect(fakeClient.discardSideChat).toHaveBeenCalledWith("side:1");
    expect(screen.queryByText(/Side chat ·/)).toBeNull();
    // The item itself stays open — Discuss is offered again, fresh.
    expect(screen.getByRole("button", { name: /^Discuss$/ })).toBeTruthy();
  });
});
