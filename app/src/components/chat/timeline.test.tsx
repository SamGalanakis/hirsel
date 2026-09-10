import { flush } from "solid-js";
import { fireEvent, render, within } from "@solidjs/testing-library";
import { createSignal } from "solid-js";

import { describe, expect, it } from "vitest";
import type { TurnEvent } from "../../protocol";
import type { TimelineEvent } from "../../store/types";
import { setShowAgentCode } from "../../lib/prefs";
import { Timeline } from "./Timeline";
import { buildTimeline, isReasoningTail } from "./timeline";

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
  it("renders a prose block before the tool row (seq order preserved)", () => {
    const { container, getByText } = render(() => (
      <Timeline
        events={evs(
          { kind: "prose", text: "First I'll read the file." },
          { kind: "tool_start", id: "t1", name: "read_file", summary: "x.ts", input: null },
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
      <Timeline events={evs({ kind: "tool_start", id: "t1", name: "read_file", summary: "x.ts", input: null })} />
    ));
    expect(running.container.querySelector('[aria-label="running"]')).toBeTruthy();
    expect(running.container.querySelector('[aria-label="ok"]')).toBeNull();

    const done = render(() => (
      <Timeline
        events={evs(
          { kind: "tool_start", id: "t1", name: "read_file", summary: "x.ts", input: null },
          { kind: "tool_done", id: "t1", name: "read_file", ok: true, summary: "read 10 lines", result: null },
        )}
      />
    ));
    expect(done.container.querySelector('[aria-label="ok"]')).toBeTruthy();
    expect(done.getByText("x.ts · Succeeded")).toBeTruthy();
  });

  it("keeps reasoning readable inline without repeated disclosure chrome", () => {
    const { container, getByText } = render(() => (
      <Timeline events={evs({ kind: "reasoning", text: "secret chain of thought" })} />
    ));
    const row = container.querySelector('[data-slot="timeline-reasoning"]') as HTMLElement;
    expect(getByText("secret chain of thought")).toBeTruthy();
    expect(within(row).queryByRole("button")).toBeNull();
    expect(row.textContent).not.toContain("reasoning");
  });

  it("renders inline markdown in reasoning without literal asterisks", () => {
    const { container } = render(() => (
      <Timeline events={evs({ kind: "reasoning", text: "**Inspecting** the resolve path" })} />
    ));
    const row = container.querySelector('[data-slot="timeline-reasoning"]') as HTMLElement;
    expect(row.querySelector("strong")?.textContent).toBe("Inspecting");
    // The dim italic block carries the full prose but never the raw `**` markers.
    const block = row.querySelector("p") as HTMLElement;
    expect(block.textContent).toBe("Inspecting the resolve path");
    expect(block.textContent).not.toContain("*");
  });

  it("expands a resolved tool row to reveal its full result in a mono well", () => {
    const { container, queryByText, getByText } = render(() => (
      <Timeline
        events={evs(
          { kind: "tool_start", id: "t1", name: "grep", summary: "TODO", input: null },
          { kind: "tool_done", id: "t1", name: "grep", ok: true, summary: "line 1\nline 2\nline 3", result: null },
        )}
      />
    ));
    const row = container.querySelector('[data-slot="timeline-tool"]') as HTMLElement;
    const toggle = within(row).getByRole("button");
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(row.querySelector("pre")).toBeNull();
    fireEvent.click(toggle);
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    // The full untruncated payload is now in a mono <pre> well.
    const well = row.querySelector("pre") as HTMLElement;
    expect(well).toBeTruthy();
    expect(well.textContent).toContain("line 3");
    expect(getByText("grep")).toBeTruthy();
    expect(queryByText("secret")).toBeNull();
  });

  it("keeps a distinct shell command and plain outcome collapsed, then prioritizes output while preserving raw result", () => {
    const { container } = render(() => (
      <Timeline events={evs(
        { kind: "tool_start", id: "shell", name: "shell_run", summary: "cmd: printf", input: { text: "{\n  \"cmd\": \"printf hello\"\n}", truncated: false } },
        { kind: "tool_done", id: "shell", name: "shell_run", ok: true, summary: "ok status 0", result: { text: "{\n  \"outcome\": {\n    \"payload\": {\n      \"status\": 0,\n      \"stderr\": \"\",\n      \"stdout\": \"hello\",\n      \"timed_out\": false\n    },\n    \"status\": \"success\"\n  }\n}", truncated: true } },
      )} />
    ));
    const row = container.querySelector('[data-slot="timeline-tool"]') as HTMLElement;
    expect(row.textContent).toContain("shell_run");
    expect(row.textContent).toContain("cmd: printf");
    expect(row.textContent).toContain("Succeeded");
    expect(row.textContent).not.toContain("ok status 0");
    fireEvent.click(within(row).getByRole("button"));
    const payload = row.querySelector('[data-slot="tool-result"]') as HTMLElement;
    const primary = payload.querySelector(":scope > pre") as HTMLElement;
    expect(primary.textContent).toContain("Output");
    expect(primary.textContent).toContain("hello");
    expect(payload.textContent).toContain("Input");
    expect(payload.textContent).toContain("printf hello");
    const raw = payload.querySelector('[data-slot="tool-result-raw"]') as HTMLDetailsElement;
    expect(raw.open).toBe(false);
    expect(raw.textContent).toContain('"stdout": "hello"');
    expect(payload.textContent).toContain("… result truncated");
  });

  it("keeps completed shell rows distinguishable by their bounded start summaries", () => {
    const { container } = render(() => <Timeline events={evs(
      { kind: "tool_start", id: "first", name: "shell_run", summary: "cmd: printf first", input: null },
      { kind: "tool_done", id: "first", name: "shell_run", ok: true, summary: "ok status 0", result: null },
      { kind: "tool_start", id: "second", name: "shell_run", summary: "cmd: printf second", input: null },
      { kind: "tool_done", id: "second", name: "shell_run", ok: true, summary: "ok status 0", result: null },
    )} />);
    const rows = [...container.querySelectorAll('[data-slot="timeline-tool"]')];
    expect(rows.map(row => row.textContent)).toEqual(expect.arrayContaining([
      expect.stringContaining("cmd: printf first · Succeeded"),
      expect.stringContaining("cmd: printf second · Succeeded"),
    ]));
    expect(rows[0].textContent).not.toBe(rows[1].textContent);
  });

  it("shows a per-tool duration from the client arrival timestamps", () => {
    const withTiming = [
      { seq: 1, at: 1000, event: { kind: "tool_start", id: "t1", name: "read_file", summary: "x.ts", input: null } },
      { seq: 2, at: 3400, event: { kind: "tool_done", id: "t1", name: "read_file", ok: true, summary: "ok", result: null } },
    ] as const;
    const { container } = render(() => <Timeline events={withTiming as never} />);
    const row = container.querySelector('[data-slot="timeline-tool"]') as HTMLElement;
    // 2400ms → "2.4s".
    expect(row.textContent).toContain("2.4s");
  });

  it("marks a delegation/sub-agent tool row with a distinct glyph", () => {
    const { container } = render(() => (
      <Timeline events={evs({ kind: "tool_start", id: "d1", name: "spawn_subagent", summary: "review", input: null })} />
    ));
    const row = container.querySelector('[data-slot="timeline-tool"]') as HTMLElement;
    expect(row.querySelector('[aria-label="delegation"]')).toBeTruthy();
  });

  it("does not mark a plain read_file as a delegation", () => {
    const { container } = render(() => (
      <Timeline events={evs({ kind: "tool_start", id: "t1", name: "read_file", summary: "x.ts", input: null })} />
    ));
    const row = container.querySelector('[data-slot="timeline-tool"]') as HTMLElement;
    expect(row.querySelector('[aria-label="delegation"]')).toBeNull();
  });

  it("hides agent code cells unless the local preference is on", () => {
    const events = evs(
      { kind: "code_start", id: "c1", language: "typescript", code: "finish(1);", truncated: false },
      { kind: "code_done", id: "c1", ok: true, summary: "12ms" },
    );
    expect(buildTimeline(events).map((i) => i.kind)).toEqual([]);
    const shown = buildTimeline(events, true);
    expect(shown).toHaveLength(1);
    expect(shown[0]).toMatchObject({
      kind: "code",
      language: "typescript",
      code: "finish(1);",
      status: { state: "done", ok: true, result: "12ms" },
    });
  });

  it("renders the full program in a collapsed cell once the preference is on", () => {
    flush(() => setShowAgentCode(true));
    try {
      const source = "const out = await shell.run({ cmd: \"true\" });\nfinish(out);";
      const { container, getByRole, queryByText } = render(() => (
        <Timeline
          events={evs({
            kind: "code_start",
            id: "c1",
            language: "typescript",
            code: source,
            truncated: false,
          })}
        />
      ));
      const row = container.querySelector('[data-slot="timeline-code"]') as HTMLElement;
      expect(row).toBeTruthy();
      // Collapsed by default: the source is not in the DOM until expanded.
      expect(queryByText(source)).toBeNull();
      fireEvent.click(getByRole("button", { name: /show source/ }));
      expect(row.textContent).toContain("finish(out);");
    } finally {
      flush(() => setShowAgentCode(false));
    }
  });
});

/** The committed bubble renders the frozen turn through the very same
 * `Timeline`, so a turn that ran an Agent program keeps its code cell after the
 * commit — and the Settings toggle governs it there too, live. */
describe("committed turn details: agent code cells", () => {
  const frozen = evs(
    { kind: "prose", text: "Running a cell." },
    {
      kind: "code_start",
      id: "code:1",
      language: "typescript",
      code: "finish(await subagents_list());",
      truncated: false,
    },
    { kind: "tool_start", id: "t1", name: "subagents_list", summary: "", input: null },
    { kind: "tool_done", id: "t1", name: "subagents_list", ok: true, summary: "2 agents", result: null },
    { kind: "code_done", id: "code:1", ok: true, summary: "34ms" },
  );

  it("shows the code cell in a committed turn when the preference is on", () => {
    flush(() => setShowAgentCode(true));
    try {
      const { container, getByRole } = render(() => <Timeline events={frozen} />);
      const cell = container.querySelector('[data-slot="timeline-code"]') as HTMLElement;
      expect(cell).toBeTruthy();
      expect(cell.textContent).toContain("typescript");
      fireEvent.click(getByRole("button", { name: /show source/ }));
      expect(cell.textContent).toContain("subagents_list()");
    } finally {
      flush(() => setShowAgentCode(false));
    }
  });

  it("hides it when the preference is off, keeping the tool row", () => {
    const { container } = render(() => <Timeline events={frozen} />);
    expect(container.querySelector('[data-slot="timeline-code"]')).toBeNull();
    expect(container.querySelector('[data-slot="timeline-tool"]')).toBeTruthy();
  });

  it("reacts to the toggle without re-committing the message", () => {
    const { container } = render(() => <Timeline events={frozen} />);
    expect(container.querySelector('[data-slot="timeline-code"]')).toBeNull();
    try {
      flush(() => setShowAgentCode(true));
      expect(container.querySelector('[data-slot="timeline-code"]')).toBeTruthy();
      flush(() => setShowAgentCode(false));
      expect(container.querySelector('[data-slot="timeline-code"]')).toBeNull();
    } finally {
      flush(() => setShowAgentCode(false));
    }
  });
});

describe("streaming reasoning block", () => {
  const thinking = { kind: "reasoning", text: "Checking the resolve path first." } as const;
  const stream = (c: HTMLElement) => c.querySelector('[data-slot="timeline-reasoning-stream"]');
  const row = (c: HTMLElement) => c.querySelector('[data-slot="timeline-reasoning"]');

  it("streams the live tail inline, with none of the reasoning row chrome", () => {
    const { container, getByText } = render(() => <Timeline events={evs(thinking)} live />);
    expect(getByText("Checking the resolve path first.")).toBeTruthy();
    // Bare text: no disclosure row, and so nothing to click.
    expect(row(container)).toBeNull();
    expect(stream(container)).toBeTruthy();
    expect(within(stream(container) as HTMLElement).queryByRole("button")).toBeNull();
    expect(container.textContent).not.toContain("reasoning");
  });

  it("clamps the streaming block's height and keeps its tail in view", () => {
    const { container } = render(() => <Timeline events={evs(thinking)} live />);
    const block = stream(container) as HTMLElement;
    // Bottom-anchored (`flex-col-reverse`) so overflow spills off the top —
    // the newest text stays visible as the run grows.
    expect(block.className).toContain("flex-col-reverse");
    expect(block.className).toContain("overflow-hidden");
    expect(block.className).toContain("max-h-[9.75em]");
    // No indent: the live block sits inline in the timeline.
    expect(block.className).not.toContain("pl-4");
  });

  it("keeps earlier reasoning inline once a later event follows the run", () => {
    const { container, getByText } = render(() => (
      <Timeline
        events={evs(thinking, { kind: "tool_start", id: "t1", name: "grep", summary: "resolve", input: null })}
        live
      />
    ));
    expect(stream(container)).toBeNull();
    expect(within(row(container) as HTMLElement).queryByRole("button")).toBeNull();
    expect(getByText("Checking the resolve path first.")).toBeTruthy();
  });

  it("keeps the reasoning visible when the turn stops being live", () => {
    const [live, setLive] = createSignal(true);
    const { container, getByText } = render(() => <Timeline events={evs(thinking)} live={live()} />);
    expect(stream(container)).toBeTruthy();
    flush(() => setLive(false));
    expect(stream(container)).toBeNull();
    expect(getByText("Checking the resolve path first.")).toBeTruthy();
  });

  it("renders committed reasoning inline, never as the live block", () => {
    const { container, getByText } = render(() => (
      <Timeline events={evs(thinking)} />
    ));
    expect(stream(container)).toBeNull();
    expect(within(row(container) as HTMLElement).queryByRole("button")).toBeNull();
    expect(getByText("Checking the resolve path first.")).toBeTruthy();
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
