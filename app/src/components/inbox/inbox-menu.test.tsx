import { render, waitFor, within } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { Ping } from "../../protocol";
import { PingCard } from "./PingCard";

// Kobalte's dropdown content renders into a Portal on document.body, so it is
// outside the render container the returned queries are bound to. Query the
// menu through `within(document.body)`; the trigger lives inside the card.

function item(overrides: Partial<Ping> = {}): Ping {
  return {
    id: 1,
    name: "deploy-approval",
    description: "Approve the deployment",
    content: "Approve the deploy?",
    anchor: 5,
    requires_response: false,
    quick_replies: [],
    status: "open",
    ts: "2026-07-08T00:00:00Z",
    ...overrides,
  };
}

function renderCard(overrides: Partial<Ping> = {}) {
  const onResolve = vi.fn();
  const onRead = vi.fn();
  const onMarkUnread = vi.fn();
  const onJumpToChat = vi.fn();
  const onSendReply = vi.fn();
  const onDiscuss = vi.fn();
  const screen = render(() => (
    <PingCard
      ping={item(overrides)}
      onSendReply={onSendReply}
      onJumpToChat={onJumpToChat}
      onResolve={onResolve}
      onRead={onRead}
      onMarkUnread={onMarkUnread}
      onDiscuss={onDiscuss}
    />
  ));
  return { screen, onResolve, onRead, onMarkUnread, onJumpToChat };
}

const menu = () => within(document.body);

async function openMenu(screen: ReturnType<typeof render>) {
  const user = userEvent.setup();
  await user.click(screen.getByLabelText("More actions"));
  return user;
}

describe("Inbox card ⋯ context menu", () => {
  it("Mark done sends the resolve action (wire resolve_ping) via onResolve", async () => {
    const { screen, onResolve } = renderCard({ id: 42 });
    const user = await openMenu(screen);
    await user.click(await menu().findByRole("menuitem", { name: "Mark done" }));
    await waitFor(() => expect(onResolve).toHaveBeenCalledTimes(1));
    expect(onResolve.mock.calls[0][0]).toMatchObject({ id: 42 });
  });

  it("there is no inline Mark done/Delete affordance outside the ⋯ menu", () => {
    const { screen } = renderCard();
    expect(screen.queryByRole("button", { name: /archive|delete/i })).toBeNull();
    // The ⋯ menu is closed by default, so "Mark done" is not in the document yet.
    expect(menu().queryByText("Mark done")).toBeNull();
  });

  it("an unread card offers Mark read (→ onRead)", async () => {
    const { screen, onRead } = renderCard({ id: 1, read: false });
    const user = await openMenu(screen);
    await user.click(await menu().findByRole("menuitem", { name: "Mark read" }));
    await waitFor(() => expect(onRead).toHaveBeenCalledTimes(1));
    expect(menu().queryByText("Mark unread")).toBeNull();
  });

  it("a read card offers Mark unread (→ onMarkUnread, client-only)", async () => {
    const { screen, onMarkUnread } = renderCard({ id: 2, read: true });
    const user = await openMenu(screen);
    await user.click(await menu().findByRole("menuitem", { name: "Mark unread" }));
    await waitFor(() => expect(onMarkUnread).toHaveBeenCalledTimes(1));
    expect(menu().queryByText("Mark read")).toBeNull();
  });

  it("View in chat jumps to the anchor via onJumpToChat", async () => {
    const { screen, onJumpToChat } = renderCard({ id: 3 });
    const user = await openMenu(screen);
    await user.click(await menu().findByRole("menuitem", { name: "View in chat" }));
    await waitFor(() => expect(onJumpToChat).toHaveBeenCalledTimes(1));
  });
});
