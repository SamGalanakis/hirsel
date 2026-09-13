import { Bot, Braces, Check, LoaderCircle, Square, Wrench, X } from "@/components/ui/icons";
import { createMemo, createSignal, For, Match, Show, Switch } from "solid-js";

import { formatDuration } from "../../lib/duration";
import type { TimelineEvent } from "../../store/types";
import { CodeBlock } from "../markdown/CodeBlock";
import { Markdown, renderInline } from "../Markdown";
import { buildTimeline, isStepRow, type StepRowItem, type StepStatus, type TimelineItem, type ToolItem } from "./timeline";

/** A tool whose job is to hand real work to a sub-agent reads differently from a
 * plain `read_file`: it earns a distinct glyph and a touch more weight so the
 * "show the work" delegation is legible at a glance (design: delegation is a
 * first-class event in the timeline, not another line-noise tool row). Matched
 * on the tool name since the wire carries no explicit "is a delegation" flag. */
const DELEGATION_RE = /(delegat|spawn|sub[_-]?agent|dispatch_agent|run_agent|^task$)/i;
function isDelegationTool(name: string): boolean {
  return DELEGATION_RE.test(name);
}

/** The live reasoning block's ceiling: six lines at the trace's own measure.
 * Enough that streaming thought reads as a block of text rather than a ticker,
 * never enough for a long chain to push the reply — or the composer — off
 * screen.
 *
 * `flex-col-reverse` is what keeps the TAIL in view as the run grows: the child
 * is laid out from the bottom edge, so overflow spills off the TOP, which is the
 * text already read. */
const LIVE_REASONING_BLOCK =
  "flex max-h-[9.75em] flex-col-reverse overflow-hidden";

/** The reasoning run the Agent is writing right now: bare dim text in the
 * timeline, with none of the settled row's chrome — no disclosure chevron, no
 * glyph, no "reasoning" label, no indent. There is nothing to disclose while the
 * text is arriving, and a label plus a toggle around three lines of live thought
 * is furniture around the only thing worth reading. Height-clamped and
 * tail-anchored so a long chain never dominates the screen.
 *
 * It is deliberately a different component from `ReasoningRow`: only the live
 * tail needs a height ceiling and tail anchoring. */
function StreamingReasoning(props: { text: string }) {
  return (
    <li
      class={`my-1 w-full min-w-0 ${LIVE_REASONING_BLOCK}`}
      data-slot="timeline-reasoning-stream"
      aria-busy="true"
    >
      <p class="max-w-prose whitespace-pre-wrap text-xs text-muted-foreground">
        {renderInline(props.text)}
      </p>
    </li>
  );
}

/** Settled reasoning remains readable in the stream. Tool rows already divide
 * long thought into chronological blocks; repeating a labelled disclosure
 * around every block obscures the actual sequence. */
function ReasoningRow(props: { text: string }) {
  return (
    <li class="my-1 w-full min-w-0" data-slot="timeline-reasoning">
      <p class="max-w-prose whitespace-pre-wrap text-xs text-muted-foreground">
        {renderInline(props.text)}
      </p>
    </li>
  );
}

interface ToolResultPresentation {
  primary: string;
  raw: string | null;
}

/** Shell results have one stable Host-owned envelope. Keep the common payload
 * readable without turning every tool result into a generic JSON inspector;
 * the exact bounded wire value remains available as the secondary raw result. */
function presentToolResult(name: string, result: string | null, truncated: boolean): ToolResultPresentation | null {
  if (result === null) return null;
  if (name !== "shell_run") return { primary: result, raw: null };
  try {
    const decoded = JSON.parse(result) as { outcome?: { payload?: unknown } };
    const value = decoded.outcome?.payload;
    if (value === null || typeof value !== "object" || Array.isArray(value)) return { primary: result, raw: null };
    const payload = value as Record<string, unknown>;
    const sections: string[] = [];
    if (typeof payload.stdout === "string" && payload.stdout.length > 0) sections.push(`Output\n${payload.stdout}`);
    if (typeof payload.stderr === "string" && payload.stderr.length > 0) sections.push(`Error output\n${payload.stderr}`);
    if (typeof payload.status === "number" || typeof payload.status === "string") sections.push(`Exit status\n${payload.status}`);
    if (payload.timed_out === true) sections.push("Timed out");
    if (sections.length === 0) return { primary: result, raw: null };
    if (truncated) sections.push("… result truncated");
    return { primary: sections.join("\n\n"), raw: result };
  } catch {
    return { primary: result, raw: null };
  }
}

/** Every step wears the same four states, so a glance down the row reads as one
 * alphabet: running, ok, failed, and "started but never reported". */
function StatusGlyph(props: { status: StepStatus; settled?: boolean }) {
  const done = () => (props.status.state === "done" ? props.status : null);
  return (
    <Switch>
      <Match when={!done() && props.settled}><Square class="size-3 shrink-0" aria-label="no result" /></Match>
      <Match when={!done()}><LoaderCircle class="size-3 shrink-0 animate-spin text-status-active" aria-label="running" /></Match>
      <Match when={done()?.ok}><Check class="size-3 shrink-0 text-status-success" aria-label="ok" /></Match>
      <Match when={done()}><X class="size-3 shrink-0 text-destructive" aria-label="failed" /></Match>
    </Switch>
  );
}

/** The one shape every step wears: a FLAT row, the whole width of the trace,
 * one per line — t3code's work rows, not a capsule. The bordered pills this
 * replaced each carried a rounded outline, a fill and a 24px height, so eight
 * tool calls read as eight competing objects wrapping across the card instead
 * of one scannable column of work. Chrome is now hover-only; failure tints the
 * text, and the open step takes the quiet fill so the panel below is
 * unambiguously its. */
const ROW = "flex w-full min-w-0 items-center gap-1.5 rounded-md px-0.5 py-0.5 text-left text-meta transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring pointer-coarse:min-h-9";
function rowClass(state: { failed?: boolean; open?: boolean }): string {
  if (state.failed) return `${ROW} text-destructive hover:bg-destructive/10`;
  if (state.open) return `${ROW} bg-muted text-foreground`;
  return `${ROW} text-muted-foreground hover:bg-accent/40 hover:text-foreground`;
}
/** The row's own summary takes whatever width is left and truncates; the timing
 * is pinned to the right edge so a column of steps reads as a column of times.
 * The slot holds a measured duration or nothing: a live turn's events are
 * stamped on arrival, a replayed timeline carries no clock and shows none. */
const ROW_DETAIL = "min-w-0 flex-1 truncate";
const ROW_TIME = "ml-auto shrink-0 pl-1.5 tabular-nums text-muted-foreground/70";

/** One tool call as a pill: outcome, kind, name, the condensed argument summary
 * and its measured duration. It opens the shared detail panel below the row. */
function ToolPill(props: { item: ToolItem; settled?: boolean; open: boolean; onToggle: () => void }) {
  const done = () => (props.item.status.state === "done" ? props.item.status : null);
  const detail = () => toolDetail(props.item);
  const running = () => done() === null && !props.settled;
  const delegation = () => isDelegationTool(props.item.name);
  const duration = () => {
    const ms = done()?.durationMs;
    return ms === undefined || ms === null ? "" : formatDuration(ms);
  };
  const body = () => (
    <>
      <StatusGlyph status={props.item.status} settled={props.settled} />
      <Show when={delegation()} fallback={<Wrench class="size-3 shrink-0 text-muted-foreground/70" aria-hidden="true" />}>
        <Bot class="size-3 shrink-0" aria-label="delegation" />
      </Show>
      <span class={["shrink-0 font-mono", { "text-foreground": running() || delegation(), "font-medium": delegation() }]}>{props.item.name}</span>
      {/* Always present, even empty: it is the spring that pins the time. */}
      <span class={ROW_DETAIL}>{detail() ?? ""}</span>
      <Show when={duration()}><span class={ROW_TIME}>{duration()}</span></Show>
      <Show when={!done() && props.settled}><span class="shrink-0">No result recorded</span></Show>
    </>
  );
  return (
    <li class="w-full min-w-0" data-slot="timeline-tool" data-tool-call-id={props.item.toolId}>
      {/* A pill with nothing behind it is a label, not a dead affordance. */}
      <Show when={toolPayload(props.item).length > 0} fallback={<span class={rowClass({ failed: done()?.ok === false })}>{body()}</span>}>
        <button
          type="button"
          class={rowClass({ failed: done()?.ok === false, open: props.open })}
          aria-expanded={props.open ? "true" : "false"}
          aria-label={`${props.item.name} — ${props.open ? "hide" : "show"} result`}
          onClick={props.onToggle}
        >
          {body()}
        </button>
      </Show>
    </li>
  );
}

/** The one line of a collapsed cell: the first statement the program runs, so
 * the pill says what the cell is without opening it. */
function programPreview(code: string): string {
  const line = code.split("\n").map(part => part.trim()).find(part => part.length > 0 && !part.startsWith("//")) ?? "";
  return line.length > 90 ? `${line.slice(0, 89).trimEnd()}…` : line;
}

/** One Agent program cell, a peer of the tool pills beside it. The tools the
 * cell called sit next to it in arrival order rather than under it: the Owner
 * reads one flat sequence of work, not a tree. */
function CodePill(props: { item: Extract<TimelineItem, { kind: "code" }>; settled?: boolean; open: boolean; onToggle: () => void }) {
  const done = () => (props.item.status.state === "done" ? props.item.status : null);
  const running = () => done() === null && !props.settled;
  const language = () => props.item.language || null;
  const duration = () => {
    const ms = done()?.durationMs;
    return ms === undefined || ms === null ? "" : formatDuration(ms);
  };
  // The collapsed cell names itself by its first statement; its output lives
  // in the panel, never in the summary slot where it read as a duration.
  const detail = () => programPreview(props.item.code);
  const body = () => (
    <>
      <StatusGlyph status={props.item.status} settled={props.settled} />
      <Braces class="size-3 shrink-0" aria-hidden="true" />
      <span class={["shrink-0 font-mono", { "text-foreground": running() }]}>Code</span>
      <Show when={language()}><span class="shrink-0 font-mono text-muted-foreground">{language()}</span></Show>
      <span class={ROW_DETAIL}>{detail() ?? ""}</span>
      <Show when={duration()}><span class={ROW_TIME}>{duration()}</span></Show>
    </>
  );
  return (
    <li class="w-full min-w-0" data-slot="timeline-code" data-code-id={props.item.codeId}>
      <Show when={props.item.code.length > 0} fallback={<span class={rowClass({ failed: done()?.ok === false })}>{body()}</span>}>
        <button
          type="button"
          class={rowClass({ failed: done()?.ok === false, open: props.open })}
          aria-expanded={props.open ? "true" : "false"}
          aria-label={`Code — ${props.open ? "hide" : "show"} source`}
          onClick={props.onToggle}
        >
          {body()}
        </button>
      </Show>
    </li>
  );
}

/** An opaque identifier — a UUID, a hex handle — names nothing to the Owner;
 * a row carrying one as its summary says less than a row carrying nothing. */
const OPAQUE_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$|^[0-9a-f]{16,}$/i;
/** What a collapsed tool row says about itself: its bounded start summary (the
 * argument that tells two calls of one tool apart), else the tool's own
 * outcome summary, else nothing — and never a bare id. The outcome itself is
 * the glyph's word, not this slot's: "Succeeded" printed after every summary
 * said nothing the mark had not. */
function toolDetail(item: ToolItem): string | null {
  const outcome = item.status.state === "done" ? item.status : null;
  const readable = (text: string | null | undefined) => text && !OPAQUE_ID.test(text.trim()) ? text : null;
  return readable(item.summary) ?? (outcome ? readable(outcome.summary?.replace(/^(?:ok|err)\s+/i, "")) : null);
}
/** Everything the open panel shows for a tool: its result, then its input. */
function toolPayload(item: ToolItem): string {
  const outcome = item.status.state === "done" ? item.status : null;
  const sections: string[] = [];
  const presentation = presentToolResult(item.name, outcome?.result ?? null, outcome?.resultTruncated ?? false);
  if (presentation !== null) sections.push(`${presentation.primary}${outcome?.resultTruncated && presentation.raw === null ? "\n… result truncated" : ""}`);
  else if (outcome?.summary) sections.push(`Result\n${outcome.summary}`);
  if (item.input !== null) sections.push(`Input\n${item.input}${item.inputTruncated ? "\n… truncated" : ""}`);
  return sections.join("\n\n") || toolDetail(item) || "";
}

/** A tool's own panel: the readable payload, with the bounded raw shell
 * envelope kept secondary underneath it. */
function ToolDetail(props: { item: ToolItem; failed: boolean }) {
  const done = () => (props.item.status.state === "done" ? props.item.status : null);
  const presentation = () => presentToolResult(props.item.name, done()?.result ?? null, done()?.resultTruncated ?? false);
  return (
    <div data-slot="tool-result" data-tool-call-id={props.item.toolId} class="space-y-1.5">
      <pre class={["max-h-64 overflow-auto whitespace-pre-wrap wrap-break-word border-l border-border/60 pl-2 font-mono text-meta text-foreground/80", { "text-destructive/90": props.failed }]}>
        {toolPayload(props.item)}
      </pre>
      <Show when={presentation()?.raw}>{raw =>
        <details data-slot="tool-result-raw" class="text-meta text-muted-foreground">
          <summary class="min-h-11 cursor-pointer py-3">Raw result{done()?.resultTruncated ? " (truncated)" : ""}</summary>
          <pre class="max-h-64 overflow-auto whitespace-pre-wrap wrap-break-word border-l border-border/60 pl-2 font-mono text-foreground/75">{raw()}</pre>
        </details>
      }</Show>
    </div>
  );
}

/** The single panel the open pill reveals, rendered below the whole row so the
 * pills never reflow into a tree. One step is open at a time. */
function StepDetail(props: { item: StepRowItem }) {
  const failed = () => props.item.status.state === "done" && !props.item.status.ok;
  return (
    <li class="w-full min-w-0" data-slot="timeline-detail">
      <Switch>
        <Match when={props.item.kind === "code" ? (props.item as Extract<TimelineItem, { kind: "code" }>) : null}>{cell =>
          <div class={["min-w-0", { "text-destructive/90": failed() }]}>
            <CodeBlock code={cell().code} lang={cell().language || null} wrap bare />
            <Show when={cell().truncated}>
              <p class="px-1 pt-1 text-meta text-muted-foreground">… truncated by the host</p>
            </Show>
          </div>
        }</Match>
        <Match when={props.item.kind === "tool" ? (props.item as ToolItem) : null}>{tool => <ToolDetail item={tool()} failed={failed()} />}</Match>
      </Switch>
    </li>
  );
}

/**
 * The running (or finished) turn rendered as a lash-CLI-style timeline: prose
 * blocks interleaved with tool rows and readable reasoning, in exact seq order.
 * Prose is muted vs committed bubbles so the live turn reads as provisional.
 * Drives both the live view and the committed inline activity stream. `live`
 * tells the actively growing reasoning tail from settled history.
 */
export function Timeline(props: { events: TimelineEvent[]; live?: boolean; settled?: boolean }) {
  // Durations (tool_done.at − tool_start.at) come out of the fold on each row's
  // status, measured within that row's own id namespace.
  const items = createMemo(() => buildTimeline(props.events));
  const [openKey, setOpenKey] = createSignal<string | null>(null);
  const toggle = (key: string) => setOpenKey(current => (current === key ? null : key));
  const openItem = createMemo(() => items().find(item => item.key === openKey() && isStepRow(item)) as StepRowItem | undefined);
  /** The panel belongs below the whole run of pills the open step sits in, so
   * opening one never splits the row it is part of. */
  const detailAnchor = createMemo(() => {
    const rows = items();
    const open = openItem();
    if (!open) return null;
    let index = rows.indexOf(open);
    while (index + 1 < rows.length && isStepRow(rows[index + 1])) index += 1;
    return rows[index].key;
  });
  // Only the LAST item of a live turn is still being written. A reasoning run
  // there is the Agent thinking at this instant, so its height is bounded while
  // streaming. Settled reasoning stays inline at full length.
  const streamingKey = createMemo(() => {
    if (!props.live) return null;
    const rows = items();
    const last = rows[rows.length - 1];
    return last?.kind === "reasoning" ? last.key : null;
  });
  return (
    <ul
      /* One step per line, a hairline apart: t3code's work log, where the
         column of names and the column of timings both read top to bottom.
         The old row wrapped capsules inline, so a long run reflowed into a
         paragraph of pills whose order was only recoverable by reading. */
      class="ml-1 flex min-w-0 flex-col gap-px border-l border-border/60 pl-3"
      data-slot="timeline"
    >
      <For each={items()} keyed={item => item.key}>
        {(row) => (
          <>
            <Switch>
              <Match when={row().kind === "prose"}>
                <li class="my-1 w-full min-w-0" data-slot="timeline-prose">
                  <Markdown class="text-muted-foreground">
                    {(row() as Extract<TimelineItem, { kind: "prose" }>).text}
                  </Markdown>
                </li>
              </Match>
              <Match when={row().kind === "reasoning"}>
                {/* Live tail and settled history share the same quiet prose
                    treatment; only the actively growing tail is height-bounded. */}
                <Show
                  when={row().key === streamingKey()}
                  fallback={
                    <ReasoningRow text={(row() as Extract<TimelineItem, { kind: "reasoning" }>).text} />
                  }
                >
                  <StreamingReasoning
                    text={(row() as Extract<TimelineItem, { kind: "reasoning" }>).text}
                  />
                </Show>
              </Match>
              <Match when={row().kind === "tool"}>
                <ToolPill item={row() as ToolItem} settled={props.settled} open={openKey() === row().key} onToggle={() => toggle(row().key)} />
              </Match>
              <Match when={row().kind === "code"}>
                <CodePill item={row() as Extract<TimelineItem, { kind: "code" }>} settled={props.settled} open={openKey() === row().key} onToggle={() => toggle(row().key)} />
              </Match>
            </Switch>
            <Show when={row().key === detailAnchor() && openItem()}>{item => <StepDetail item={item()} />}</Show>
          </>
        )}
      </For>
    </ul>
  );
}
