import { describe, expect, it } from "vitest";
import type { TurnEvent } from "../../protocol";
import type { TimelineEvent } from "../../store/types";
import { buildTimeline, isReasoningTail, splitStreamingReply, timelineTools } from "./timeline";

function evs(...events: TurnEvent[]): TimelineEvent[] {
  return events.map((event, i) => ({ seq: i + 1, event }));
}

describe("buildTimeline (fold)", () => {
  it("accumulates consecutive prose deltas into one block", () => {
    const items = buildTimeline(evs({ kind: "prose", text: "Hello " }, { kind: "prose", text: "world" }));
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({ kind: "prose", text: "Hello world" });
  });

  it("keeps adjacent reasoning blocks separate while joining chunks within each block", () => {
    const items = buildTimeline(evs(
      { kind: "reasoning", block_id: "reasoning-1", text: "**First " },
      { kind: "reasoning", block_id: "reasoning-1", text: "thought.**" },
      { kind: "reasoning", block_id: "reasoning-2", text: "**Second thought.**" },
    ));

    expect(items).toMatchObject([
      { kind: "reasoning", text: "**First thought.**" },
      { kind: "reasoning", text: "**Second thought.**" },
    ]);
  });

  it("keeps provisional prose blocks distinct in the streaming reply", () => {
    const split = splitStreamingReply(evs(
      { kind: "prose", block_id: "prose-1", text: "First " },
      { kind: "prose", block_id: "prose-1", text: "paragraph." },
      { kind: "prose", block_id: "prose-2", text: "Second paragraph." },
    ));

    expect(split.activity).toEqual([]);
    expect(split.reply).toBe("First paragraph.\n\nSecond paragraph.");
  });

  it("splits the prose block at a tool_start and reopens after it", () => {
    const items = buildTimeline(
      evs(
        { kind: "prose", text: "before" },
        { kind: "tool_start", id: "t1", name: "read_file", summary: "x.ts", input: null },
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
        { kind: "tool_start", id: "t1", name: "grep", summary: "TODO", input: null },
        { kind: "tool_done", id: "t1", name: "grep", ok: true, summary: "3 matches", result: null },
      ),
    );
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({
      kind: "tool",
      status: { state: "done", ok: true, summary: "3 matches", result: null },
    });
  });

  it("renders an orphan tool_done as a completed row using its own name", () => {
    const items = buildTimeline(evs({ kind: "tool_done", id: "ghost", name: "grep", ok: false, summary: "no match", result: null }));
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({
      kind: "tool",
      name: "grep",
      summary: null,
      status: { state: "done", ok: false, summary: "no match", result: null },
    });
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
  it("folds an agent code cell into the timeline as a peer of the tools it called", () => {
    const events = evs(
      { kind: "code_start", id: "c1", language: "typescript", code: "finish(1);", truncated: false },
      { kind: "tool_start", id: "t1", name: "read_file", summary: "x.ts", input: null },
      { kind: "tool_done", id: "t1", name: "read_file", ok: true, summary: "read", result: null },
      { kind: "code_done", id: "c1", ok: true, summary: "12ms" },
      { kind: "tool_start", id: "t2", name: "read_file", summary: "y.ts", input: null },
    );
    const items = buildTimeline(events);
    // One flat sequence in arrival order: the cell, the tool it ran, then the
    // tool that ran after it closed. Nothing is nested.
    expect(items.map(i => i.kind)).toEqual(["code", "tool", "tool"]);
    expect(items[0]).toMatchObject({
      kind: "code",
      language: "typescript",
      code: "finish(1);",
      status: { state: "done", ok: true, result: "12ms" },
    });
    expect(items[1]).toMatchObject({ kind: "tool", toolId: "t1", status: { state: "done", ok: true } });
    expect(items[2]).toMatchObject({ kind: "tool", toolId: "t2", status: { state: "running" } });
    // Every tool row is reachable in order.
    expect(timelineTools(items).map(tool => tool.toolId)).toEqual(["t1", "t2"]);
  });

  it("drops a cell whose whole program is one finish call", () => {
    const trivial = evs(
      { kind: "code_start", id: "c1", language: "typescript", code: 'finish("")', truncated: false },
      { kind: "code_done", id: "c1", ok: true, summary: null },
    );
    expect(buildTimeline(trivial)).toEqual([]);
    // A finish carrying a literal is the prose rendered right below it: the
    // entry would only say the same thing twice.
    const spoken = evs(
      { kind: "code_start", id: "c1", language: "typescript", code: 'await finish("Your list is ready.");', truncated: false },
      { kind: "code_done", id: "c1", ok: true, summary: null },
    );
    expect(buildTimeline(spoken)).toEqual([]);
    // Anything beyond the bare finish is real work and keeps its entry.
    const real = evs(
      { kind: "code_start", id: "c1", language: "typescript", code: 'const list = await shell.run({ cmd: "ls" });\nfinish("done");', truncated: false },
      { kind: "code_done", id: "c1", ok: true, summary: null },
    );
    expect(buildTimeline(real).map(i => i.kind)).toEqual(["code"]);
    // So does a finish whose argument had to be computed.
    const computed = evs(
      { kind: "code_start", id: "c2", language: "typescript", code: "finish(await subagents_list());", truncated: false },
      { kind: "code_done", id: "c2", ok: true, summary: null },
    );
    expect(buildTimeline(computed).map(i => i.kind)).toEqual(["code"]);
  });
});

describe("isReasoningTail (thinking-marker gate)", () => {
  it("is true when the turn's last act is a reasoning delta", () => {
    expect(isReasoningTail(evs({ kind: "prose", text: "ok" }, { kind: "reasoning", text: "hm" }))).toBe(true);
  });

  it("is false for a tool tail, and for an empty turn", () => {
    expect(
      isReasoningTail(
        evs({ kind: "reasoning", text: "hm" }, { kind: "tool_start", id: "t1", name: "grep", summary: "x", input: null }),
      ),
    ).toBe(false);
    expect(isReasoningTail([])).toBe(false);
  });
});
