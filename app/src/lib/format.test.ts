import { describe, expect, it } from "vitest";
import { formatDayGroup, formatEventAge, formatReturnTime } from "./format";

const NOW = new Date(2026, 6, 14, 12, 0, 0).getTime(); // local noon

describe("formatEventAge (time axis)", () => {
  it("is terse and relative under 24h, an absolute date past it", () => {
    expect(formatEventAge(new Date(NOW - 30_000).toISOString(), NOW)).toBe("now");
    expect(formatEventAge(new Date(NOW - 2 * 60_000).toISOString(), NOW)).toBe("2m");
    expect(formatEventAge(new Date(NOW - 6 * 3600_000).toISOString(), NOW)).toBe("6h");
    const old = new Date(NOW - 48 * 3600_000);
    expect(formatEventAge(old.toISOString(), NOW)).toBe(
      old.toLocaleDateString(undefined, { month: "short", day: "numeric" }),
    );
    expect(formatEventAge("nonsense", NOW)).toBe("");
  });
});

describe("formatReturnTime (durable snooze)", () => {
  it("labels the return with Today / Tomorrow / weekday / date", () => {
    const today = new Date(2026, 6, 14, 18, 0, 0);
    expect(formatReturnTime(today.toISOString(), NOW)).toMatch(/^Today /);
    const tomorrow = new Date(2026, 6, 15, 9, 0, 0);
    expect(formatReturnTime(tomorrow.toISOString(), NOW)).toMatch(/^Tomorrow /);
    const later = new Date(2026, 6, 17, 9, 0, 0); // 3 days out → weekday
    expect(formatReturnTime(later.toISOString(), NOW)).toMatch(/\d/);
    const nextWeek = new Date(2026, 6, 25, 9, 0, 0); // > 7 days → date
    expect(formatReturnTime(nextWeek.toISOString(), NOW)).toBe(
      nextWeek.toLocaleDateString(undefined, { month: "short", day: "numeric" }),
    );
    expect(formatReturnTime("nonsense", NOW)).toBe("");
  });
});

describe("formatDayGroup (archived day-log)", () => {
  it("groups by Today / Yesterday / full date", () => {
    expect(formatDayGroup(new Date(2026, 6, 14, 8, 0, 0).toISOString(), NOW)).toBe("Today");
    expect(formatDayGroup(new Date(2026, 6, 13, 23, 0, 0).toISOString(), NOW)).toBe("Yesterday");
    const old = new Date(2026, 6, 10, 9, 0, 0);
    expect(formatDayGroup(old.toISOString(), NOW)).toBe(
      old.toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" }),
    );
  });
});
