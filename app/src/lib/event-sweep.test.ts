import { beforeEach, describe, expect, it, vi } from "vitest";

beforeEach(() => {
  vi.resetModules();
});

describe("clearFinishedEventsWithUndo — the one-op sweep with a batch undo", () => {
  it("archives the whole batch optimistically, sends ONE clear op, and undoes the batch", async () => {
    const store = await import("../store/store");
    const toast = await import("./toast");
    let cleared = 0;
    const unarchived: number[] = [];
    vi.doMock("../ws/client", () => ({
      getClient: () => ({
        clearFinishedEvents: () => (cleared += 1),
        sendEventAction: (eventId: number, action: string) => {
          if (action === "unarchive") unarchived.push(eventId);
        },
      }),
    }));
    const { clearFinishedEventsWithUndo } = await import("./event-sweep");

    const ids = clearFinishedEventsWithUndo([1, 2, 3]);
    expect(ids).toEqual([1, 2, 3]);
    // One wire op — not three per-card archives.
    expect(cleared).toBe(1);
    // The whole batch is optimistically archived at once.
    expect(store.state.eventArchiveOverrides).toEqual([1, 2, 3]);

    // The toast reads "Cleared 3" with an Undo that unarchives exactly the batch.
    const t = toast.toasts().find((x) => /Cleared 3/.test(x.message));
    expect(t?.action?.label).toBe("Undo");
    t!.action!.onClick();
    expect(unarchived).toEqual([1, 2, 3]);
    expect(store.state.eventArchiveOverrides).toEqual([]);
  });

  it("is a no-op on an empty batch (no wire op, no toast)", async () => {
    await import("../store/store");
    const toast = await import("./toast");
    let cleared = 0;
    vi.doMock("../ws/client", () => ({
      getClient: () => ({ clearFinishedEvents: () => (cleared += 1) }),
    }));
    const { clearFinishedEventsWithUndo } = await import("./event-sweep");
    expect(clearFinishedEventsWithUndo([])).toEqual([]);
    expect(cleared).toBe(0);
    expect(toast.toasts().some((x) => /Cleared/.test(x.message))).toBe(false);
  });
});
