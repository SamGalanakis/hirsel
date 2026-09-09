import { flush } from "solid-js";
import { beforeEach, describe, expect, it, vi } from "vitest";

// The undo toast's auto-dismiss must PAUSE while the toast is hovered/focused
// and RESUME with the time it had left (spec item 7), so a user reaching for
// "Undo" never loses it. Fake timers (which also drive Date.now in vitest)
// exercise the countdown; fresh module singleton per test.
beforeEach(() => {
  vi.resetModules();
  vi.useFakeTimers();
});

describe("toast auto-dismiss pause/resume", () => {
  it("dismisses on the original window when never paused", async () => {
    const { toast, toasts } = await import("./toast");
    toast("Marked done", { durationMs: 4000 });
    flush();
    expect(toasts()).toHaveLength(1);
    vi.advanceTimersByTime(3999);
    flush();
    expect(toasts()).toHaveLength(1);
    vi.advanceTimersByTime(1);
    flush();
    expect(toasts()).toHaveLength(0);
  });

  it("pausing halts the countdown; it dismisses only after resume + remaining", async () => {
    const { toast, toasts, pauseToast, resumeToast } = await import("./toast");
    const id = toast("Marked done", { durationMs: 5000 });
    flush();

    vi.advanceTimersByTime(2000); // 3s left
    flush();
    pauseToast(id);
    flush();
    vi.advanceTimersByTime(60_000); // paused: the clock is frozen, no dismiss
    flush();
    expect(toasts()).toHaveLength(1);

    resumeToast(id);
    flush();
    vi.advanceTimersByTime(2999); // just shy of the remaining 3s
    flush();
    expect(toasts()).toHaveLength(1);
    vi.advanceTimersByTime(1);
    flush();
    expect(toasts()).toHaveLength(0);
  });

  it("pause/resume are idempotent and safe after dismissal", async () => {
    const { toast, toasts, pauseToast, resumeToast, dismissToast } = await import("./toast");
    const id = toast("hi", { durationMs: 3000 });
    flush();
    pauseToast(id);
    flush();
    pauseToast(id); // no-op
    flush();
    resumeToast(id);
    flush();
    resumeToast(id); // no-op
    flush();
    dismissToast(id);
    flush();
    expect(toasts()).toHaveLength(0);
    // No throw / no resurrection.
    resumeToast(id);
    flush();
    vi.advanceTimersByTime(10_000);
    flush();
    expect(toasts()).toHaveLength(0);
  });
});
