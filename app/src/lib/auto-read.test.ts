import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createSeenTimer, SEEN_DELAY_MS } from "./auto-read";

describe("createSeenTimer — email-like 'seen' decision", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("fires onSeen after the item is continuously visible for the delay", () => {
    const onSeen = vi.fn();
    const t = createSeenTimer({ onSeen });

    t.setVisible(true);
    expect(onSeen).not.toHaveBeenCalled();
    vi.advanceTimersByTime(SEEN_DELAY_MS - 1);
    expect(onSeen).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(onSeen).toHaveBeenCalledTimes(1);
  });

  it("does not fire if the item scrolls out of view before the delay elapses", () => {
    const onSeen = vi.fn();
    const t = createSeenTimer({ onSeen });

    t.setVisible(true);
    vi.advanceTimersByTime(SEEN_DELAY_MS - 100);
    t.setVisible(false); // scrolled away in time
    vi.advanceTimersByTime(1000);
    expect(onSeen).not.toHaveBeenCalled();
  });

  it("restarts the dwell timer when visibility toggles (no early fire from summed partial dwells)", () => {
    const onSeen = vi.fn();
    const t = createSeenTimer({ onSeen });

    t.setVisible(true);
    vi.advanceTimersByTime(1000);
    t.setVisible(false);
    t.setVisible(true); // fresh dwell — needs a full delay again
    vi.advanceTimersByTime(1000);
    expect(onSeen).not.toHaveBeenCalled();
    vi.advanceTimersByTime(SEEN_DELAY_MS - 1000);
    expect(onSeen).toHaveBeenCalledTimes(1);
  });

  it("fires immediately on interaction, without waiting for the dwell", () => {
    const onSeen = vi.fn();
    const t = createSeenTimer({ onSeen });

    t.interacted();
    expect(onSeen).toHaveBeenCalledTimes(1);
  });

  it("fires onSeen at most once across dwell + interaction", () => {
    const onSeen = vi.fn();
    const t = createSeenTimer({ onSeen });

    t.setVisible(true);
    vi.advanceTimersByTime(SEEN_DELAY_MS);
    expect(onSeen).toHaveBeenCalledTimes(1);
    t.interacted(); // already seen
    t.setVisible(false);
    t.setVisible(true);
    vi.advanceTimersByTime(SEEN_DELAY_MS);
    expect(onSeen).toHaveBeenCalledTimes(1);
  });

  it("honours a custom delay", () => {
    const onSeen = vi.fn();
    const t = createSeenTimer({ onSeen, delayMs: 500 });
    t.setVisible(true);
    vi.advanceTimersByTime(499);
    expect(onSeen).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(onSeen).toHaveBeenCalledTimes(1);
  });

  it("dispose cancels a pending dwell", () => {
    const onSeen = vi.fn();
    const t = createSeenTimer({ onSeen });
    t.setVisible(true);
    t.dispose();
    vi.advanceTimersByTime(SEEN_DELAY_MS * 2);
    expect(onSeen).not.toHaveBeenCalled();
  });
});
