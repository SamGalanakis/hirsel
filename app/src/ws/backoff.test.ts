import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { backoffDelayMs, MAX_BACKOFF_MS, MIN_BACKOFF_MS } from "./backoff";

describe("backoffDelayMs", () => {
  it("starts at the minimum delay on the first attempt", () => {
    expect(backoffDelayMs(0)).toBe(MIN_BACKOFF_MS);
  });

  it("doubles each attempt", () => {
    expect(backoffDelayMs(1)).toBe(2000);
    expect(backoffDelayMs(2)).toBe(4000);
    expect(backoffDelayMs(3)).toBe(8000);
    expect(backoffDelayMs(4)).toBe(16000);
  });

  it("caps at the maximum delay", () => {
    expect(backoffDelayMs(5)).toBe(MAX_BACKOFF_MS);
    expect(backoffDelayMs(20)).toBe(MAX_BACKOFF_MS);
  });
});

describe("reconnect scheduling with fake timers", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("schedules successive reconnect attempts at the expected backoff delays", () => {
    const attempts: number[] = [];
    let attempt = 0;

    function scheduleNext() {
      const delay = backoffDelayMs(attempt);
      setTimeout(() => {
        attempts.push(delay);
        attempt += 1;
        if (attempt < 4) scheduleNext();
      }, delay);
    }

    scheduleNext();

    vi.advanceTimersByTime(1000);
    expect(attempts).toEqual([1000]);

    vi.advanceTimersByTime(2000);
    expect(attempts).toEqual([1000, 2000]);

    vi.advanceTimersByTime(4000);
    expect(attempts).toEqual([1000, 2000, 4000]);

    vi.advanceTimersByTime(8000);
    expect(attempts).toEqual([1000, 2000, 4000, 8000]);
  });

  it("resets the attempt counter after a successful reconnect", () => {
    let attempt = 0;
    const delays: number[] = [];

    // Two failed attempts, then a "successful" reconnect resets attempt to 0.
    delays.push(backoffDelayMs(attempt++));
    delays.push(backoffDelayMs(attempt++));
    attempt = 0; // simulate successful `hello_ok`
    delays.push(backoffDelayMs(attempt));

    expect(delays).toEqual([1000, 2000, 1000]);
  });
});
