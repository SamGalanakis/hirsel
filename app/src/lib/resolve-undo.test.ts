import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Ping } from "../protocol";

// Done is reversible (spec item 2). `resolve_ping` is terminal on the wire —
// there is no reopen op (protocol.ts) — so Mark done is made recoverable
// client-side: an optimistic Done flip now, the actual send debounced behind a
// 5s Undo window, modelled on the read-flip override machinery. Fresh store +
// module singletons per test (resetModules + dynamic import), like the rest of
// the suite; fake timers drive the debounce window.
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

describe("markDoneWithUndo: optimistic flip + debounced resolve_ping", () => {
  it("flips to Done at once but defers the send, then commits when the window elapses", async () => {
    vi.useFakeTimers();
    const resolvePing = vi.fn();
    vi.doMock("../ws/client", () => ({ getClient: () => ({ resolvePing }) }));
    const store = await import("../store/store");
    const selectors = await import("../store/selectors");
    const { markDoneWithUndo, hasPendingResolve } = await import("./resolve-undo");

    store.dispatch({ type: "ping_upsert", payload: { type: "ping_upsert", ping: ping() } });
    markDoneWithUndo(1);

    // Optimistic: the Ping reads as resolved immediately, but nothing has hit
    // the host yet — the send is held.
    expect(store.state.resolveOverrides).toContain(1);
    expect(selectors.isPingResolved(store.state.pings[0], store.state.resolveOverrides)).toBe(true);
    expect(resolvePing).not.toHaveBeenCalled();
    expect(hasPendingResolve(1)).toBe(true);

    // Window elapses with no Undo -> the resolve commits exactly once.
    vi.advanceTimersByTime(5000);
    expect(resolvePing).toHaveBeenCalledTimes(1);
    expect(resolvePing).toHaveBeenCalledWith(1);
    expect(hasPendingResolve(1)).toBe(false);

    // The override lingers (no flicker) until the host's `done` ping_upsert
    // lands, then the reducer prunes it.
    expect(store.state.resolveOverrides).toContain(1);
    store.dispatch({
      type: "ping_upsert",
      payload: { type: "ping_upsert", ping: ping({ status: "done" }) },
    });
    expect(store.state.resolveOverrides).not.toContain(1);
  });

  it("Undo inside the window cancels the send and returns the Ping to open", async () => {
    vi.useFakeTimers();
    const resolvePing = vi.fn();
    vi.doMock("../ws/client", () => ({ getClient: () => ({ resolvePing }) }));
    const store = await import("../store/store");
    const selectors = await import("../store/selectors");
    const { markDoneWithUndo, undoDone, hasPendingResolve } = await import("./resolve-undo");

    store.dispatch({ type: "ping_upsert", payload: { type: "ping_upsert", ping: ping() } });
    markDoneWithUndo(1);
    expect(store.state.resolveOverrides).toContain(1);

    undoDone(1);
    // Back to open, no pending send, and the window firing later is a no-op.
    expect(store.state.resolveOverrides).not.toContain(1);
    expect(selectors.isPingResolved(store.state.pings[0], store.state.resolveOverrides)).toBe(false);
    expect(hasPendingResolve(1)).toBe(false);
    vi.advanceTimersByTime(6000);
    expect(resolvePing).not.toHaveBeenCalled();
  });
});
