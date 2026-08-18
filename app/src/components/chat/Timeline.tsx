import { Bot, Braces, Brain, Check, ChevronRight, ListTree, LoaderCircle, X } from "lucide-solid";
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

/** A collapsed reasoning run: a thin, dim "reasoning" row that expands to the
 * dim italic text. Kept deliberately quieter than prose — never a headline. */
function ReasoningRow(props: { text: string }) {
  const [open, setOpen] = createSignal(false);
  return (
    <li class="flex flex-col gap-1" data-slot="timeline-reasoning">
      <button
        type="button"
        class="inline-flex w-fit items-center gap-1 text-[0.72rem] text-muted-foreground/70 transition-colors hover:text-muted-foreground"
        aria-expanded={open()}
        onClick={() => setOpen((v) => !v)}
      >
        <ChevronRight
          class="size-3 shrink-0 transition-transform"
          classList={{ "rotate-90": open() }}
          aria-hidden="true"
        />
        <Brain class="size-3 shrink-0" aria-hidden="true" />
        <span class="italic">reasoning</span>
      </button>
      <Show when={open()}>
        <p class="whitespace-pre-wrap pl-4 text-[0.72rem] italic leading-relaxed text-muted-foreground/60">
          {renderInline(props.text)}
        </p>
      </Show>
    </li>
  );
}

/** One resolved/pending tool row. While running it is the emphasized step
 * (spinner + full-strength name); once done it quiets down (dimmer name) so the
 * live cursor is always the running step. Carries a quiet right-aligned mono
 * duration once resolved, and — when it produced a result/error — click-to-
 * expand into a mono "well" showing the full, untruncated payload. */
function ToolRow(props: {
  item: Extract<TimelineItem, { kind: "tool" }>;
  durationMs: number | null;
}) {
  const [open, setOpen] = createSignal(false);
  const detail = () => props.item.result ?? props.item.summary;
  const hasDetail = () => (detail() ?? "").length > 0;
  const running = () => !props.item.done;
  const delegation = () => isDelegationTool(props.item.name);
  const duration = () => (props.durationMs !== null ? formatDuration(props.durationMs) : "");

  return (
    <li class="flex min-w-0 flex-col gap-1" data-slot="timeline-tool">
      <div class="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
        <Switch>
          <Match when={running()}>
            <LoaderCircle
              class="size-3 shrink-0 animate-spin text-status-active"
              aria-label="running"
            />
          </Match>
          <Match when={props.item.ok}>
            <Check class="size-3 shrink-0 text-status-success" aria-label="ok" />
          </Match>
          <Match when={!props.item.ok}>
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
              class="shrink-0 font-mono text-[0.72rem]"
              classList={{
                "text-foreground": running() || delegation(),
                "text-foreground/70": !running() && !delegation(),
                "font-medium": delegation(),
              }}
            >
              {props.item.name}
            </span>
          }
        >
          <button
            type="button"
            class="flex min-w-0 flex-1 items-center gap-1.5 text-left"
            aria-expanded={open()}
            aria-label={`${props.item.name} — ${open() ? "hide" : "show"} result`}
            onClick={() => setOpen((v) => !v)}
          >
            <ChevronRight
              class="size-3 shrink-0 text-muted-foreground/60 transition-transform"
              classList={{ "rotate-90": open() }}
              aria-hidden="true"
            />
            <span
              class="shrink-0 font-mono text-[0.72rem]"
              classList={{
                "text-foreground": running() || delegation(),
                "text-foreground/70": !running() && !delegation(),
                "font-medium": delegation(),
              }}
            >
              {props.item.name}
            </span>
            <span class="min-w-0 flex-1 truncate">{detail()}</span>
          </button>
        </Show>
        <Show when={duration()}>
          <span class="ml-auto shrink-0 pl-1 font-mono text-xs tabular-nums text-muted-foreground/60">
            {duration()}
          </span>
        </Show>
      </div>
      <Show when={open() && hasDetail()}>
        <pre
          class="ml-4 max-h-64 overflow-auto whitespace-pre-wrap wrap-break-word rounded-md bg-muted/50 px-2 py-1.5 font-mono text-[0.72rem] leading-relaxed text-foreground/80"
          classList={{ "text-destructive/90": props.item.done && props.item.ok === false }}
        >
          {detail()}
        </pre>
      </Show>
    </li>
  );
}

/** One Agent program cell (Settings → "Show agent code"). Collapsed by default
 * to a single quiet row — the source is the exception you open, not the thing
 * you read every turn — expanding to the verbatim monospace program. Once the
 * cell completes, a failure tints the row and the well so a broken program is
 * findable without expanding it. */
function CodeRow(props: {
  item: Extract<TimelineItem, { kind: "code" }>;
  durationMs: number | null;
}) {
  const [open, setOpen] = createSignal(false);
  const running = () => !props.item.done;
  const failed = () => props.item.done && props.item.ok === false;
  const label = () => props.item.language || "code";
  const hasCode = () => props.item.code.length > 0;
  const duration = () => (props.durationMs !== null ? formatDuration(props.durationMs) : "");

  return (
    <li class="flex min-w-0 flex-col gap-1" data-slot="timeline-code">
      <div class="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
        <Switch>
          <Match when={running()}>
            <LoaderCircle
              class="size-3 shrink-0 animate-spin text-status-active"
              aria-label="running"
            />
          </Match>
          <Match when={props.item.ok}>
            <Check class="size-3 shrink-0 text-status-success" aria-label="ok" />
          </Match>
          <Match when={!props.item.ok}>
            <X class="size-3 shrink-0 text-destructive" aria-label="failed" />
          </Match>
        </Switch>
        <Show
          when={hasCode()}
          fallback={
            <span class="shrink-0 font-mono text-[0.72rem] text-foreground/70">{label()}</span>
          }
        >
          <button
            type="button"
            class="flex min-w-0 flex-1 items-center gap-1.5 text-left"
            aria-expanded={open()}
            aria-label={`${label()} program — ${open() ? "hide" : "show"} source`}
            onClick={() => setOpen((v) => !v)}
          >
            <ChevronRight
              class="size-3 shrink-0 text-muted-foreground/60 transition-transform"
              classList={{ "rotate-90": open() }}
              aria-hidden="true"
            />
            <Braces class="size-3 shrink-0" aria-hidden="true" />
            <span
              class="shrink-0 font-mono text-[0.72rem]"
              classList={{ "text-foreground": running(), "text-foreground/70": !running() }}
            >
              {label()}
            </span>
            <Show when={props.item.result}>
              <span class="min-w-0 flex-1 truncate" classList={{ "text-destructive/90": failed() }}>
                {props.item.result}
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
          class="ml-4 max-h-96 overflow-auto whitespace-pre rounded-md bg-muted/50 px-2 py-1.5 font-mono text-[0.72rem] leading-relaxed text-foreground/80"
          classList={{ "text-destructive/90": failed() }}
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
 * blocks interleaved with tool rows and collapsed reasoning, in exact seq order.
 * Prose is muted vs committed bubbles so the live turn reads as provisional.
 * Drives both the live view under the thinking marker and the committed "turn
 * details" panel.
 */
export function Timeline(props: { events: TimelineEvent[] }) {
  const items = createMemo(() => buildTimeline(props.events, showAgentCode()));
  // Per-tool duration from the client-stamped arrival times (tool_done.at −
  // tool_start.at). Absent when either endpoint predates timing (hand-built
  // events / replayed data), in which case the row simply shows no duration.
  const durations = createMemo(() => {
    const starts = new Map<string, number>();
    const out = new Map<string, number>();
    for (const { event, at } of props.events) {
      if (at === undefined) continue;
      if (event.kind === "tool_start" || event.kind === "code_start") starts.set(event.id, at);
      else if (event.kind === "tool_done" || event.kind === "code_done") {
        const start = starts.get(event.id);
        if (start !== undefined) out.set(event.id, at - start);
      }
    }
    return out;
  });
  return (
    <ul
      class="ml-1 flex min-w-0 flex-col gap-2 border-l border-border/60 pl-3"
      data-slot="timeline"
    >
      <For each={items()}>
        {(item) => (
          <Switch>
            <Match when={item.kind === "prose"}>
              <li class="min-w-0" data-slot="timeline-prose">
                <Markdown class="text-muted-foreground">
                  {(item as Extract<TimelineItem, { kind: "prose" }>).text}
                </Markdown>
              </li>
            </Match>
            <Match when={item.kind === "reasoning"}>
              <ReasoningRow text={(item as Extract<TimelineItem, { kind: "reasoning" }>).text} />
            </Match>
            <Match when={item.kind === "tool"}>
              {(() => {
                const tool = item as Extract<TimelineItem, { kind: "tool" }>;
                return <ToolRow item={tool} durationMs={durations().get(tool.toolId) ?? null} />;
              })()}
            </Match>
            <Match when={item.kind === "code"}>
              {(() => {
                const cell = item as Extract<TimelineItem, { kind: "code" }>;
                return <CodeRow item={cell} durationMs={durations().get(cell.codeId) ?? null} />;
              })()}
            </Match>
          </Switch>
        )}
      </For>
    </ul>
  );
}

/** Committed-turn "turn details" affordance (v1.5) in an agent bubble's footer:
 * a subtle collapsed chip that expands inline to the finished turn's full
 * timeline. Supersedes the "⚙ N tools" chip whenever a live timeline was
 * captured for the turn (it already conveys the tool outcomes, and more).
 *
 * Expansion is optionally controllable (`expanded`/`onExpandedChange`) so a
 * message's actions menu ("View turn details") can force it open; uncontrolled
 * (self-managed) when those props are omitted. */
export function TurnDetails(props: {
  events: TimelineEvent[];
  expanded?: boolean;
  onExpandedChange?: (open: boolean) => void;
}) {
  const [internal, setInternal] = createSignal(false);
  const expanded = () => props.expanded ?? internal();
  const toggle = () => {
    const next = !expanded();
    setInternal(next);
    props.onExpandedChange?.(next);
  };
  return (
    <div class="flex flex-col gap-1" data-slot="turn-details">
      <button
        type="button"
        class="-ml-1 inline-flex w-fit items-center gap-1 rounded px-1 py-px text-xs text-muted-foreground transition-colors hover:bg-muted/40 hover:text-foreground"
        aria-expanded={expanded()}
        aria-label="Turn details"
        onClick={toggle}
      >
        <ChevronRight
          class="size-3 shrink-0 transition-transform"
          classList={{ "rotate-90": expanded() }}
          aria-hidden="true"
        />
        <ListTree class="size-3 shrink-0" aria-hidden="true" />
        turn details
      </button>
      <Show when={expanded()}>
        <Timeline events={props.events} />
      </Show>
    </div>
  );
}
