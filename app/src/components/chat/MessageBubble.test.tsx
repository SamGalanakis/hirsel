import { render, waitFor, within } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { DisplayMessage } from "../../store/types";
import { MessageBubble } from "./MessageBubble";

// Kobalte's dropdown content portals onto document.body (like the Inbox ⋯ menu),
// so query the menu through `within(document.body)`.
const menu = () => within(document.body);

function msg(overrides: Partial<DisplayMessage> = {}): DisplayMessage {
  return {
    id: 10,
    author: "agent",
    body: "Here is the answer.",
    ref: null,
    ts: "2026-07-09T09:30:00Z",
    attachments: [],
    ...overrides,
  };
}

function renderBubble(overrides: Partial<DisplayMessage> = {}, props: Partial<Parameters<typeof MessageBubble>[0]> = {}) {
  const onReply = vi.fn();
  const screen = render(() => (
    <MessageBubble
      message={msg(overrides)}
      refTarget={undefined}
      showQuote={false}
      highlighted={false}
      queued={false}
      onReply={onReply}
      onTapQuote={vi.fn()}
      onOpenImage={vi.fn()}
      onRetry={vi.fn()}
      onCancelQueued={vi.fn()}
      {...props}
    />
  ));
  return { screen, onReply };
}

describe("MessageBubble transcript grammar", () => {
  it("renders an agent message as canvas prose (ghost bubble, no chip fill)", () => {
    const { screen } = renderBubble({ author: "agent" });
    const bubble = screen.container.querySelector('[data-slot="bubble"]') as HTMLElement;
    expect(bubble.getAttribute("data-variant")).toBe("ghost");
    expect(screen.getByText("Here is the answer.")).toBeTruthy();
  });

  it("renders an owner message as a compact right-aligned accent chip", () => {
    const { screen } = renderBubble({ author: "owner", body: "do the thing" });
    const bubble = screen.container.querySelector('[data-slot="bubble"]') as HTMLElement;
    expect(bubble.getAttribute("data-variant")).toBe("default");
    const message = screen.container.querySelector('[data-slot="message"]') as HTMLElement;
    expect(message.getAttribute("data-align")).toBe("end");
  });
});

describe("MessageBubble per-message actions", () => {
  it('"Reply" from the actions menu quotes the message via onReply', async () => {
    const { screen, onReply } = renderBubble({ id: 42, author: "agent" });
    const user = userEvent.setup();
    await user.click(screen.getByLabelText("Message actions"));
    await user.click(await menu().findByRole("menuitem", { name: "Reply" }));
    await waitFor(() => expect(onReply).toHaveBeenCalledWith(42));
  });

  it("offers Copy, and offers View turn details only when a turn timeline exists", async () => {
    const { screen } = renderBubble(
      { id: 7, author: "agent" },
      { turnDetails: [{ seq: 1, event: { kind: "prose", text: "worked on it" } }] },
    );
    const user = userEvent.setup();
    await user.click(screen.getByLabelText("Message actions"));
    expect(await menu().findByRole("menuitem", { name: "Copy" })).toBeTruthy();
    expect(await menu().findByRole("menuitem", { name: "View turn details" })).toBeTruthy();
  });

  it("hides View turn details when there is no captured timeline", async () => {
    const { screen } = renderBubble({ id: 8, author: "agent" });
    const user = userEvent.setup();
    await user.click(screen.getByLabelText("Message actions"));
    await menu().findByRole("menuitem", { name: "Copy" });
    expect(menu().queryByRole("menuitem", { name: "View turn details" })).toBeNull();
  });
});

describe("MessageBubble clustering meta", () => {
  const TIME_RE = /\d{1,2}:\d{2}/;

  it("shows the timestamp at a cluster boundary (showMeta=true, the default)", () => {
    const { screen } = renderBubble({ author: "owner" });
    expect(TIME_RE.test(screen.container.textContent ?? "")).toBe(true);
  });

  it("suppresses the timestamp for a clustered message (showMeta=false)", () => {
    const { screen } = renderBubble({ author: "owner" }, { showMeta: false });
    expect(TIME_RE.test(screen.container.textContent ?? "")).toBe(false);
  });
});
