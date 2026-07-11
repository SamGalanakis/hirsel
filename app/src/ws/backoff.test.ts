import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { backoffDelayMs, jitteredDelayMs, MAX_BACKOFF_MS, MIN_BACKOFF_MS } from "./backoff";

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

describe("jitteredDelayMs", () => {
  it("spans exactly ±20% of the base across the rng range", () => {
    // rng=0 → 0.8×base (floor), rng→1 → 1.2×base (open ceiling).
    expect(jitteredDelayMs(2, () => 0)).toBe(Math.round(4000 * 0.8));
    expect(jitteredDelayMs(2, () => 0.999999)).toBe(Math.round(4000 * (0.8 + 0.4 * 0.999999)));
    expect(jitteredDelayMs(2, () => 0.5)).toBe(4000); // midpoint = base
  });

  it("keeps every sample within ±20% of the exponential base", () => {
    for (let attempt = 0; attempt <= 8; attempt++) {
      const base = backoffDelayMs(attempt);
      for (const r of [0, 0.13, 0.5, 0.87, 0.9999]) {
        const d = jitteredDelayMs(attempt, () => r);
        expect(d).toBeGreaterThanOrEqual(Math.round(base * 0.8));
        expect(d).toBeLessThanOrEqual(Math.round(base * 1.2));
      }
    }
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
