import { describe, expect, it } from "vitest";
import { taskIdFromPath, taskPath } from "./deep-link";

describe("/t/<id> deep links", () => {
  it("round-trips a focused Task", () => {
    expect(taskPath(12)).toBe("/t/12");
    expect(taskIdFromPath(taskPath(12))).toBe(12);
  });

  it("addresses ambient as the root, not as a place", () => {
    expect(taskPath(null)).toBe("/");
    expect(taskIdFromPath("/")).toBeNull();
  });

  it("tolerates a trailing slash and refuses anything else", () => {
    expect(taskIdFromPath("/t/3/")).toBe(3);
    expect(taskIdFromPath("/t/")).toBeNull();
    expect(taskIdFromPath("/t/abc")).toBeNull();
    expect(taskIdFromPath("/t/0")).toBeNull();
    expect(taskIdFromPath("/tasks/3")).toBeNull();
    expect(taskIdFromPath("/t/3/extra")).toBeNull();
  });
});
