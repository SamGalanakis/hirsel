import { Bot, Braces, Check, ChevronRight, LoaderCircle, Square, X } from "@/components/ui/icons";
import { createMemo, createSignal, For, Match, Show, Switch } from "solid-js";

import { showAgentCode } from "../../lib/prefs";
import type { TimelineEvent } from "../../store/types";
import { Markdown, renderInline } from "../Markdown";
import { buildTimeline, type TimelineItem } from "./timeline";

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
      class={`min-w-0 ${LIVE_REASONING_BLOCK}`}
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
    <li class="min-w-0" data-slot="timeline-reasoning">
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

/** One resolved/pending tool row. While running it is the emphasized step
 * (spinner + full-strength name); once done it quiets down (dimmer name) so the
 * live cursor is always the running step. Carries a quiet right-aligned mono
 * duration once resolved, and — when it produced a result/error — click-to-
 * expand into a readable payload with the bounded raw shell envelope secondary. */
function ToolRow(props: { item: Extract<TimelineItem, { kind: "tool" }>; settled?: boolean }) {
  const [open, setOpen] = createSignal(false);
  const done = () => (props.item.status.state === "done" ? props.item.status : null);
  const result = () => presentToolResult(props.item.name, done()?.result ?? null, done()?.resultTruncated ?? false);
  const detail = () => {
    const outcome = done();
    if (!outcome) return props.item.summary;
    const identity = props.item.summary ?? outcome.summary?.replace(/^(?:ok|err)\s+/i, "") ?? null;
    return [identity, outcome.ok ? "Succeeded" : "Failed"].filter(Boolean).join(" · ");
  };
  const primaryPayload = () => {
    const sections: string[] = [];
    const presentation = result();
    if (presentation !== null) sections.push(presentation.primary);
    else if (done()?.summary) sections.push(`Result\n${done()!.summary}`);
    if (props.item.input !== null) sections.push(`Input\n${props.item.input}${props.item.inputTruncated ? "\n… truncated" : ""}`);
    return sections.join("\n\n") || detail() || "";
  };
  const hasDetail = () => primaryPayload().length > 0;
  const running = () => done() === null && !props.settled;
  const failed = () => done()?.ok === false;
  const delegation = () => isDelegationTool(props.item.name);
  const duration = () => {
    const ms = done()?.durationMs;
    return ms === undefined || ms === null ? "" : formatDuration(ms);
  };

  return (
    <li class="flex min-w-0 flex-col gap-1" data-slot="timeline-tool" data-tool-call-id={props.item.toolId}>
      <div class="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
        <Switch>
          <Match when={!done() && props.settled}><Square class="size-3 shrink-0" aria-label="no result" /></Match>
          <Match when={running()}>
            <LoaderCircle
              class="size-3 shrink-0 animate-spin text-status-active"
              aria-label="running"
            />
          </Match>
          <Match when={done()?.ok}>
            <Check class="size-3 shrink-0 text-status-success" aria-label="ok" />
          </Match>
          <Match when={done()}>
            <X class="size-3 shrink-0 text-destructive" aria-label="failed" />
          </Match>
        </Switch>
        <Show when={delegation()}>
          <Bot class="size-3 shrink-0 text-muted-foreground" aria-label="delegation" />
        </Show>
        {/* The name + summary are a toggle when there is a payload to reveal; a
            plain span otherwise (no dead affordance). */}
        <Show
          when={hasDetail()}
          fallback={
            <span
              class={["shrink-0 font-mono text-meta", {
                "text-foreground": running() || delegation(),
                "text-foreground/70": !running() && !delegation(),
                "font-medium": delegation(),
              }]}

            >
              {props.item.name}
            </span>
          }
        >
          <button
            type="button"
            class="flex min-h-11 min-w-0 flex-1 items-center gap-1.5 rounded text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            aria-expanded={(open()) ? "true" : "false"}
            aria-label={`${props.item.name} — ${open() ? "hide" : "show"} result`}
            onClick={() => setOpen((v) => !v)}
          >
            <ChevronRight
              class={["size-3 shrink-0 text-muted-foreground/60 transition-transform", { "rotate-90": open() }]}

              aria-hidden="true"
            />
            <span
              class={["shrink-0 font-mono text-meta", {
                "text-foreground": running() || delegation(),
                "text-foreground/70": !running() && !delegation(),
                "font-medium": delegation(),
              }]}

            >
              {props.item.name}
            </span>
            <Show when={!open()}><span class="min-w-0 flex-1 truncate">{detail()}</span></Show>
          </button>
        </Show>
        <Show when={duration()}>
          <span class="ml-auto shrink-0 pl-1 font-mono text-xs tabular-nums text-muted-foreground/60">
            {duration()}
          </span>
        </Show>
        <Show when={!done() && props.settled}><span>No result recorded</span></Show>
      </div>
      <Show when={open() && hasDetail()}>
        <div data-slot="tool-result" data-tool-call-id={props.item.toolId} class="ml-4 space-y-1.5">
          <pre class={["max-h-64 overflow-auto whitespace-pre-wrap wrap-break-word rounded-md bg-muted/50 px-2 py-1.5 font-mono text-meta leading-relaxed text-foreground/80", { "text-destructive/90": failed() }]}>
            {primaryPayload()}
          </pre>
          <Show when={result()?.raw}>{raw =>
            <details data-slot="tool-result-raw" class="text-meta text-muted-foreground">
              <summary class="min-h-11 cursor-pointer py-3">Raw result{done()?.resultTruncated ? " (truncated)" : ""}</summary>
              <pre class="max-h-64 overflow-auto whitespace-pre-wrap wrap-break-word rounded-md bg-muted/35 px-2 py-1.5 font-mono leading-relaxed text-foreground/75">{raw()}</pre>
            </details>
          }</Show>
        </div>
      </Show>
    </li>
  );
}

/** One Agent program cell (Settings → "Show agent code"). Collapsed by default
 * to a single quiet row — the source is the exception you open, not the thing
 * you read every turn — expanding to the verbatim monospace program. Once the
 * cell completes, a failure tints the row and the well so a broken program is
 * findable without expanding it. */
function CodeRow(props: { item: Extract<TimelineItem, { kind: "code" }>; settled?: boolean }) {
  const [open, setOpen] = createSignal(false);
  const done = () => (props.item.status.state === "done" ? props.item.status : null);
  const running = () => done() === null && !props.settled;
  const failed = () => done()?.ok === false;
  const label = () => props.item.language || "code";
  const hasCode = () => props.item.code.length > 0;
  const duration = () => {
    const ms = done()?.durationMs;
    return ms === undefined || ms === null ? "" : formatDuration(ms);
  };

  return (
    <li class="flex min-w-0 flex-col gap-1" data-slot="timeline-code">
      <div class="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
        <Switch>
          <Match when={!done() && props.settled}><Square class="size-3 shrink-0" aria-label="no result" /></Match>
          <Match when={running()}>
            <LoaderCircle
              class="size-3 shrink-0 animate-spin text-status-active"
              aria-label="running"
            />
          </Match>
          <Match when={done()?.ok}>
            <Check class="size-3 shrink-0 text-status-success" aria-label="ok" />
          </Match>
          <Match when={done()}>
            <X class="size-3 shrink-0 text-destructive" aria-label="failed" />
          </Match>
        </Switch>
        <Show
          when={hasCode()}
          fallback={
            <span class="shrink-0 font-mono text-meta text-foreground/70">{label()}</span>
          }
        >
          <button
            type="button"
            class="flex min-h-11 min-w-0 flex-1 items-center gap-1.5 rounded text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            aria-expanded={(open()) ? "true" : "false"}
            aria-label={`${label()} program — ${open() ? "hide" : "show"} source`}
            onClick={() => setOpen((v) => !v)}
          >
            <ChevronRight
              class={["size-3 shrink-0 text-muted-foreground/60 transition-transform", { "rotate-90": open() }]}

              aria-hidden="true"
            />
            <Braces class="size-3 shrink-0" aria-hidden="true" />
            <span
              class={["shrink-0 font-mono text-meta", { "text-foreground": running(), "text-foreground/70": !running() }]}

            >
              {label()}
            </span>
            <Show when={done()?.result}>
              <span class={["min-w-0 flex-1 truncate", { "text-destructive/90": failed() }]} >
                {done()?.result}
              </span>
            </Show>
          </button>
        </Show>
        <Show when={duration()}>
          <span class="ml-auto shrink-0 pl-1 font-mono text-xs tabular-nums text-muted-foreground/60">
            {duration()}
          </span>
        </Show>
      </div>
      <Show when={open() && hasCode()}>
        <pre
          class={["ml-4 max-h-96 overflow-auto whitespace-pre rounded-md bg-muted/50 px-2 py-1.5 font-mono text-meta leading-relaxed text-foreground/80", { "text-destructive/90": failed() }]}

        >
          {props.item.code}
          <Show when={props.item.truncated}>
            <span class="text-muted-foreground/70">{"\n… truncated"}</span>
          </Show>
        </pre>
      </Show>
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
  const items = createMemo(() => buildTimeline(props.events, showAgentCode()));
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
      class="ml-1 flex min-w-0 flex-col gap-2 border-l border-border/60 pl-3"
      data-slot="timeline"
    >
      <For each={items()} keyed={item => item.key}>
        {(row) => (
          <Switch>
            <Match when={row().kind === "prose"}>
              <li class="min-w-0" data-slot="timeline-prose">
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
              <ToolRow item={row() as Extract<TimelineItem, { kind: "tool" }>} settled={props.settled} />
            </Match>
            <Match when={row().kind === "code"}>
              <CodeRow item={row() as Extract<TimelineItem, { kind: "code" }>} settled={props.settled} />
            </Match>
          </Switch>
        )}
      </For>
    </ul>
  );
}
