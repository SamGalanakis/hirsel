import { describe, expect, it } from "vitest";
import { datetimeLocalToIso, snoozePresets } from "./snooze-presets";

describe("snoozePresets", () => {
  it("offers evening / tomorrow-morning / next-week, all in the future, at local hours", () => {
    const now = new Date(2026, 6, 14, 10, 0, 0).getTime(); // local 10:00
    const presets = snoozePresets(now);
    expect(presets.map((p) => p.key)).toEqual(["evening", "tomorrow", "week"]);
    for (const p of presets) {
      expect(Date.parse(p.until)).toBeGreaterThan(now);
    }
    // This evening is 6pm local today (it is only 10am).
    expect(new Date(presets[0].until).getHours()).toBe(18);
    // Tomorrow morning is 9am local, one day out.
    const tomorrow = new Date(presets[1].until);
    expect(tomorrow.getHours()).toBe(9);
    expect(Math.round((tomorrow.getTime() - now) / 3600_000)).toBeGreaterThan(20);
    // Next week is 7 days out at 9am local.
    const week = new Date(presets[2].until);
    expect(week.getHours()).toBe(9);
    expect(Math.round((week.getTime() - now) / 86_400_000)).toBe(7);
  });

  it("rolls 'This evening' to tomorrow once the evening has passed", () => {
    const now = new Date(2026, 6, 14, 21, 0, 0).getTime(); // local 9pm — past 6pm
    const evening = new Date(snoozePresets(now)[0].until);
    expect(evening.getHours()).toBe(18);
    expect(evening.getDate()).toBe(15); // next day
  });
});

describe("datetimeLocalToIso", () => {
  it("converts a datetime-local value to an RFC3339 instant, rejecting empty/invalid", () => {
    const iso = datetimeLocalToIso("2026-07-20T09:30");
    expect(iso).not.toBeNull();
    expect(new Date(iso!).getHours()).toBe(9);
    expect(new Date(iso!).getMinutes()).toBe(30);
    expect(datetimeLocalToIso("")).toBeNull();
    expect(datetimeLocalToIso("not-a-date")).toBeNull();
  });
});
