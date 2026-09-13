import { Bot, Braces, Check, LoaderCircle, Square, Wrench, X } from "@/components/ui/icons";
import { createMemo, createSignal, For, Match, Show, Switch } from "solid-js";

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

/** Quiet, exact per-row duration. Sub-second in ms, then 1-decimal seconds, then
 * m/s — never more precision than the eye needs. */
function formatDuration(ms: number): string {
  if (ms < 0) return "";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  const s = ms / 1000;
  if (s < 10) return `${s.toFixed(1)}s`;
  if (s < 60) return `${Math.round(s)}s`;
  const m = Math.floor(s / 60);
  return `${m}m${Math.round(s % 60)}s`;
}

/** The live reasoning block's ceiling: six lines at its own italic measure
 * (`leading-relaxed` = 1.625em a line). Enough that streaming thought reads as a
 * block of text rather than a ticker, never enough for a long chain to push the
 * reply — or the composer — off screen.
 *
 * `flex-col-reverse` is what keeps the TAIL in view as the run grows: the child
 * is laid out from the bottom edge, so overflow spills off the TOP, which is the
 * text already read. */
const LIVE_REASONING_BLOCK =
  "flex max-h-[9.75em] flex-col-reverse overflow-hidden";

/** The reasoning run the Agent is writing right now: bare dim-italic text in the
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
      <p class="whitespace-pre-wrap text-meta italic leading-relaxed text-muted-foreground">
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
      <p class="max-w-prose whitespace-pre-wrap text-meta italic leading-relaxed text-muted-foreground">
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

/** The one shape every step wears: a compact bordered capsule that wraps with
 * its neighbours. Failure tints the border; the open step holds full contrast
 * so the panel below is unambiguously its. */
const PILL = "inline-flex h-6 min-w-0 max-w-full items-center gap-1.5 rounded-full border px-2 text-xs transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:h-9";
function pillClass(state: { failed?: boolean; open?: boolean }): string {
  if (state.failed) return `${PILL} border-destructive/40 bg-destructive/5 text-destructive hover:bg-destructive/10`;
  if (state.open) return `${PILL} border-border bg-muted text-foreground`;
  return `${PILL} border-border/60 bg-muted/40 text-muted-foreground hover:bg-muted hover:text-foreground`;
}
/** The trailing muted detail never widens the row past a glance's worth. */
const PILL_DETAIL = "min-w-0 max-w-56 truncate";
const PILL_TIME = "shrink-0 font-mono text-meta tabular-nums text-muted-foreground/60";

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
      <span class={["shrink-0 font-mono text-meta", { "text-foreground": running() || delegation(), "font-medium": delegation() }]}>{props.item.name}</span>
      <Show when={detail()}><span class={PILL_DETAIL}>{detail()}</span></Show>
      <Show when={duration()}><span class={PILL_TIME}>{duration()}</span></Show>
      <Show when={!done() && props.settled}><span class="shrink-0">No result recorded</span></Show>
    </>
  );
  return (
    <li class="min-w-0 max-w-full" data-slot="timeline-tool" data-tool-call-id={props.item.toolId}>
      {/* A pill with nothing behind it is a label, not a dead affordance. */}
      <Show when={toolPayload(props.item).length > 0} fallback={<span class={pillClass({ failed: done()?.ok === false })}>{body()}</span>}>
        <button
          type="button"
          class={pillClass({ failed: done()?.ok === false, open: props.open })}
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
  const detail = () => done()?.result || programPreview(props.item.code);
  const body = () => (
    <>
      <StatusGlyph status={props.item.status} settled={props.settled} />
      <Braces class="size-3 shrink-0" aria-hidden="true" />
      <span class={["shrink-0 font-mono text-meta", { "text-foreground": running() }]}>Code</span>
      <Show when={language()}><span class="shrink-0 font-mono text-meta text-muted-foreground">{language()}</span></Show>
      <Show when={detail()}><span class={PILL_DETAIL}>{detail()}</span></Show>
      <Show when={duration()}><span class={PILL_TIME}>{duration()}</span></Show>
    </>
  );
  return (
    <li class="min-w-0 max-w-full" data-slot="timeline-code" data-code-id={props.item.codeId}>
      <Show when={props.item.code.length > 0} fallback={<span class={pillClass({ failed: done()?.ok === false })}>{body()}</span>}>
        <button
          type="button"
          class={pillClass({ failed: done()?.ok === false, open: props.open })}
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

/** What a collapsed tool pill says about itself. */
function toolDetail(item: ToolItem): string | null {
  const outcome = item.status.state === "done" ? item.status : null;
  if (!outcome) return item.summary;
  const identity = item.summary ?? outcome.summary?.replace(/^(?:ok|err)\s+/i, "") ?? null;
  return [identity, outcome.ok ? "Succeeded" : "Failed"].filter(Boolean).join(" · ");
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
      <pre class={["max-h-64 overflow-auto whitespace-pre-wrap wrap-break-word border-l border-border/60 pl-2 font-mono text-meta leading-relaxed text-foreground/80", { "text-destructive/90": props.failed }]}>
        {toolPayload(props.item)}
      </pre>
      <Show when={presentation()?.raw}>{raw =>
        <details data-slot="tool-result-raw" class="text-meta text-muted-foreground">
          <summary class="min-h-11 cursor-pointer py-3">Raw result{done()?.resultTruncated ? " (truncated)" : ""}</summary>
          <pre class="max-h-64 overflow-auto whitespace-pre-wrap wrap-break-word border-l border-border/60 pl-2 font-mono leading-relaxed text-foreground/75">{raw()}</pre>
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
              <p class="px-1 pt-1 text-meta text-muted-foreground/70">… truncated by the host</p>
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
      class="ml-1 flex min-w-0 flex-wrap items-center gap-1 border-l border-border/60 pl-3"
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
