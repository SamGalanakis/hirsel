import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Ping } from "../protocol";

// Recoverable "Mark done" (spec item 2), now backed by a real `reopen_ping` op.
// Mark done flips the Ping to Done optimistically AND sends `resolve_ping`
// immediately (no debounce); the "Undo" toast — and the Done card's ⋯ "Reopen"
// — recover via `reopen_ping` + an optimistic un-flip. Fresh store + module
// singletons per test (resetModules + dynamic import), like the rest of the
// suite.
beforeEach(() => {
  vi.resetModules();
  vi.useRealTimers();
});

function ping(overrides: Partial<Ping> = {}): Ping {
  return {
    id: 1,
    name: "deploy-approval",
    description: "Approve the deploy",
    content: "Approve?",
    anchor: 5,
    requires_response: false,
    quick_replies: [],
    status: "open",
    ts: "2026-07-08T00:00:00Z",
    ...overrides,
  };
}

describe("markDoneWithUndo: optimistic flip + immediate resolve_ping", () => {
  it("flips to Done at once AND sends resolve_ping immediately — no debounce", async () => {
    const resolvePing = vi.fn();
    const reopenPing = vi.fn();
    vi.doMock("../ws/client", () => ({ getClient: () => ({ resolvePing, reopenPing }) }));
    const store = await import("../store/store");
    const selectors = await import("../store/selectors");
    const { markDoneWithUndo } = await import("./resolve-undo");

    store.dispatch({ type: "ping_upsert", payload: { type: "ping_upsert", ping: ping() } });
    markDoneWithUndo(1);

    // Optimistic Done flip.
    expect(store.state.resolveOverrides).toContain(1);
    expect(selectors.isPingResolved(store.state.pings[0], store.state.resolveOverrides)).toBe(true);
    // The resolve reaches the host NOW (never lost if the app closes mid-window).
    expect(resolvePing).toHaveBeenCalledTimes(1);
    expect(resolvePing).toHaveBeenCalledWith(1);
    expect(reopenPing).not.toHaveBeenCalled();

    // The override lingers until the host's `done` ping_upsert lands, then the
    // reducer prunes it (no flicker).
    expect(store.state.resolveOverrides).toContain(1);
    store.dispatch({
      type: "ping_upsert",
      payload: { type: "ping_upsert", ping: ping({ status: "done" }) },
    });
    expect(store.state.resolveOverrides).not.toContain(1);
  });

  it("Undo reopens the Ping via reopen_ping and optimistically un-flips it", async () => {
    const resolvePing = vi.fn();
    const reopenPing = vi.fn();
    vi.doMock("../ws/client", () => ({ getClient: () => ({ resolvePing, reopenPing }) }));
    const store = await import("../store/store");
    const selectors = await import("../store/selectors");
    const { markDoneWithUndo, undoDone } = await import("./resolve-undo");

    store.dispatch({ type: "ping_upsert", payload: { type: "ping_upsert", ping: ping() } });
    markDoneWithUndo(1);
    expect(store.state.resolveOverrides).toContain(1);

    undoDone(1);
    // Back to open locally, and a real reopen op sent to the host.
    expect(store.state.resolveOverrides).not.toContain(1);
    expect(selectors.isPingResolved(store.state.pings[0], store.state.resolveOverrides)).toBe(false);
    expect(reopenPing).toHaveBeenCalledTimes(1);
    expect(reopenPing).toHaveBeenCalledWith(1);
  });
});

describe("reopenPing: the Done card ⋯ Reopen path", () => {
  it("sends reopen_ping and drops the optimistic resolve override", async () => {
    const resolvePing = vi.fn();
    const reopenSend = vi.fn();
    vi.doMock("../ws/client", () => ({
      getClient: () => ({ resolvePing, reopenPing: reopenSend }),
    }));
    const store = await import("../store/store");
    const { reopenPing } = await import("./resolve-undo");

    store.dispatch({ type: "ping_upsert", payload: { type: "ping_upsert", ping: ping() } });
    store.dispatch({ type: "resolve_local", pingId: 1 });
    expect(store.state.resolveOverrides).toContain(1);

    reopenPing(1);
    expect(reopenSend).toHaveBeenCalledTimes(1);
    expect(reopenSend).toHaveBeenCalledWith(1);
    expect(store.state.resolveOverrides).not.toContain(1);
  });
});
