import { render, waitFor } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";

// The single-owner right-region state machine (§1 keystone). One exclusive
// `rightRegion` enum owns the desktop right pane / phone secondary sheet:
// mutual exclusivity, close-returns-to-idle, side-chat DATA outliving its pane,
// and the canvas auto-surface that must NOT steal an occupied slot. Fresh store
// singleton per test (resetModules + dynamic import), the pattern used across
// this suite.
beforeEach(() => {
  vi.resetModules();
});

describe("rightRegion: mutual exclusivity + close", () => {
  it("holds exactly one region; the last explicit intent wins", async () => {
    const store = await import("../../store/store");
    // Default resting state.
    expect(store.state.rightRegion).toBe("none");

    store.openProcesses();
    expect(store.state.rightRegion).toBe("processes");

    // Opening another pane replaces the first — nothing lingers behind it.
    store.openSettings();
    expect(store.state.rightRegion).toBe("settings");

    store.showCanvas();
    expect(store.state.rightRegion).toBe("canvas");

    // Closing any pane returns to the idle resting state.
    store.closeRightRegion();
    expect(store.state.rightRegion).toBe("none");
  });

  it("keeps the side-chat DATA alive when its pane is left (resumable)", async () => {
    const store = await import("../../store/store");

    store.openSideChat("side:1");
    expect(store.state.rightRegion).toBe("sideChat");
    expect(store.state.activeSideChatSc).toBe("side:1");

    // Leaving the pane returns the region to idle but the sc stays set — the
    // side chat is alive/resumable underneath, just not rendered.
    store.closeRightRegion();
    expect(store.state.rightRegion).toBe("none");
    expect(store.state.activeSideChatSc).toBe("side:1");
  });

  it("goToChat returns the region to idle without discarding the side-chat data", async () => {
    const store = await import("../../store/store");
    store.openSideChat("side:7");
    store.goToChat();
    expect(store.state.rightRegion).toBe("none");
    expect(store.state.activeSideChatSc).toBe("side:7");
  });
});

/** Full-App is heavier than needed; ChatView owns the canvas auto-surface
 * effect, so render it directly with a stubbed client. */
async function setupChatView() {
  const store = await import("../../store/store");
  vi.doMock("../../ws/client", () => ({
    getClient: () => ({}),
    getStoredToken: () => "tok",
    clearStoredToken: vi.fn(),
  }));
  const { ChatView } = await import("./ChatView");
  store.dispatch({ type: "connection_status", status: "connected" });
  const screen = render(() => <ChatView />);
  return { store, screen };
}

function canvasView(id: string) {
  return {
    type: "view_upsert" as const,
    payload: {
      type: "view_upsert" as const,
      instance_id: id,
      placement: "canvas" as const,
      spec: { type: "text" as const, text: `canvas ${id}` },
    },
  };
}

describe("rightRegion: the canvas auto-surface never steals an occupied slot", () => {
  it("auto-surfaces a new canvas view only from the idle state", async () => {
    const { store } = await setupChatView();

    // Occupy the region with Settings, THEN a canvas view arrives.
    store.openSettings();
    store.dispatch(canvasView("v1"));

    // It must NOT force-switch: the user stays in Settings; the availability
    // affordance (CanvasButton / overflow) surfaces it instead.
    expect(store.state.rightRegion).toBe("settings");
    expect(document.querySelector('[data-slot="canvas-rail"]')).toBeNull();

    // Back to the idle state; a genuinely NEW canvas view auto-surfaces.
    store.closeRightRegion();
    store.dispatch(canvasView("v2"));
    await waitFor(() => expect(store.state.rightRegion).toBe("canvas"));
  });
});
