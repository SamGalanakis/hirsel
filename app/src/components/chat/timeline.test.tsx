import { fireEvent, render, within } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import type { TurnEvent } from "../../protocol";
import type { TimelineEvent } from "../../store/types";
import { Timeline } from "./Timeline";
import { buildTimeline } from "./timeline";

function evs(...events: TurnEvent[]): TimelineEvent[] {
  return events.map((event, i) => ({ seq: i + 1, event }));
}

describe("buildTimeline (fold)", () => {
  it("accumulates consecutive prose deltas into one block", () => {
    const items = buildTimeline(evs({ kind: "prose", text: "Hello " }, { kind: "prose", text: "world" }));
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({ kind: "prose", text: "Hello world" });
  });

  it("splits the prose block at a tool_start and reopens after it", () => {
    const items = buildTimeline(
      evs(
        { kind: "prose", text: "before" },
        { kind: "tool_start", id: "t1", name: "read_file", summary: "x.ts" },
        { kind: "prose", text: "after" },
      ),
    );
    expect(items.map((i) => i.kind)).toEqual(["prose", "tool", "prose"]);
    expect((items[0] as { text: string }).text).toBe("before");
    expect((items[2] as { text: string }).text).toBe("after");
  });

  it("resolves a tool row in place on tool_done (does not add a row)", () => {
    const items = buildTimeline(
      evs(
        { kind: "tool_start", id: "t1", name: "grep", summary: "TODO" },
        { kind: "tool_done", id: "t1", ok: true, summary: "3 matches" },
      ),
    );
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({ kind: "tool", done: true, ok: true, result: "3 matches" });
  });

  it("drops a tool_done with no matching tool_start", () => {
    const items = buildTimeline(evs({ kind: "tool_done", id: "ghost", ok: false, summary: "?" }));
    expect(items).toEqual([]);
  });

  it("keeps reasoning runs separate from prose", () => {
    const items = buildTimeline(
      evs(
        { kind: "prose", text: "p" },
        { kind: "reasoning", text: "r1 " },
        { kind: "reasoning", text: "r2" },
        { kind: "prose", text: "p2" },
      ),
    );
    expect(items.map((i) => i.kind)).toEqual(["prose", "reasoning", "prose"]);
    expect((items[1] as { text: string }).text).toBe("r1 r2");
  });
});

describe("Timeline component", () => {
  it("renders a prose block before the tool row (seq order preserved)", () => {
    const { container, getByText } = render(() => (
      <Timeline
        events={evs(
          { kind: "prose", text: "First I'll read the file." },
          { kind: "tool_start", id: "t1", name: "read_file", summary: "x.ts" },
        )}
      />
    ));
    const list = container.querySelector('[data-slot="timeline"]')!;
    const kids = Array.from(list.children);
    // Prose li comes before the tool li.
    expect(kids[0].getAttribute("data-slot")).toBe("timeline-prose");
    expect(kids[1].getAttribute("data-slot")).toBe("timeline-tool");
    expect(getByText("First I'll read the file.")).toBeTruthy();
  });

  it("shows a spinner while a tool is running, then a check when done", () => {
    const running = render(() => (
      <Timeline events={evs({ kind: "tool_start", id: "t1", name: "read_file", summary: "x.ts" })} />
    ));
    expect(running.container.querySelector('[aria-label="running"]')).toBeTruthy();
    expect(running.container.querySelector('[aria-label="ok"]')).toBeNull();

    const done = render(() => (
      <Timeline
        events={evs(
          { kind: "tool_start", id: "t1", name: "read_file", summary: "x.ts" },
          { kind: "tool_done", id: "t1", ok: true, summary: "read 10 lines" },
        )}
      />
    ));
    expect(done.container.querySelector('[aria-label="ok"]')).toBeTruthy();
    expect(done.getByText("read 10 lines")).toBeTruthy();
  });

  it("keeps reasoning collapsed until clicked", () => {
    const { container, getByText, queryByText } = render(() => (
      <Timeline events={evs({ kind: "reasoning", text: "secret chain of thought" })} />
    ));
    const row = container.querySelector('[data-slot="timeline-reasoning"]') as HTMLElement;
    const toggle = within(row).getByRole("button");
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(queryByText("secret chain of thought")).toBeNull();
    fireEvent.click(toggle);
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    expect(getByText("secret chain of thought")).toBeTruthy();
  });
});
