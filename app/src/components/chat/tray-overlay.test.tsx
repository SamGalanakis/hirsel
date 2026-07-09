import { fireEvent, render } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";

// Tray (spec [P1] gate): expanding must overlay the message area, never push
// it — the scroller must not reflow, unmount, or lose its scroll position.
beforeEach(() => {
  vi.resetModules();
});

describe("Tray overlay never pushes Chat", () => {
  it("expanding/collapsing the Tray preserves the message scroller's node and scroll position", async () => {
    const store = await import("../../store/store");
    vi.doMock("../../ws/client", () => ({
      getClient: () => ({
        sendMessage: vi.fn(),
        resolvePing: vi.fn(),
        readPing: vi.fn(),
        cancelTurn: vi.fn(),
        retrySend: vi.fn(),
        cancelQueued: vi.fn(),
      }),
    }));

    // Seed enough messages for the scroller to render, plus an open item so
    // the shelf renders too.
    for (let i = 1; i <= 10; i++) {
      store.dispatch({
        type: "msg",
        payload: {
          type: "msg",
          message: {
            id: i,
            author: i % 2 === 0 ? "owner" : "agent",
            body: `message ${i}`,
            ref: null,
            ts: "2026-07-08T00:00:00Z",
            attachments: [],
          },
        },
      });
    }
    store.dispatch({
      type: "ping_upsert",
      payload: {
        type: "ping_upsert",
        ping: {
          id: 1,
          name: "deploy-approval",
          description: "Approve the deployment",
          content: "Approve the deploy?",
          anchor: 1,
          requires_response: true,
          quick_replies: [],
          status: "open",
          ts: "2026-07-08T00:00:00Z",
        },
      },
    });

    const { ChatView } = await import("./ChatView");
    const { container, getByLabelText } = render(() => <ChatView />);

    const viewport = container.querySelector(
      '[data-slot="message-scroller-viewport"]',
    ) as HTMLElement;
    expect(viewport).toBeTruthy();
    // Simulate the Owner having scrolled up from the bottom.
    viewport.scrollTop = 137;

    // Expand the Tray.
    fireEvent.click(getByLabelText("Open Pings"));
    expect(store.state.trayExpanded).toBe(true);

    // The scroller is untouched: same node, same scrollTop — the overlay is a
    // sibling that covers it, it never unmounts/reflows/resizes it.
    expect(
      container.querySelector('[data-slot="message-scroller-viewport"]'),
    ).toBe(viewport);
    expect(viewport.scrollTop).toBe(137);

    const panel = container.querySelector('[data-slot="tray-panel"]') as HTMLElement;
    expect(panel).toBeTruthy();
    // Out of flow — absolutely positioned over the message area, never a
    // flex/flow sibling that would push the scroller down or shrink it.
    expect(panel.className).toContain("absolute");

    // Collapse again (tap the shelf/header a second time).
    fireEvent.click(getByLabelText("Collapse Pings"));
    expect(store.state.trayExpanded).toBe(false);
    expect(container.querySelector('[data-slot="tray-panel"]')).toBeNull();
    expect(
      container.querySelector('[data-slot="message-scroller-viewport"]'),
    ).toBe(viewport);
    expect(viewport.scrollTop).toBe(137);
  });
});
