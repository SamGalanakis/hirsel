import { Brain, Check, ChevronRight, ListTree, LoaderCircle, X } from "lucide-solid";
import { createSignal, For, Match, Show, Switch } from "solid-js";
import type { TimelineEvent } from "../../store/types";
import { Markdown } from "../Markdown";
import { buildTimeline, type TimelineItem } from "./timeline";

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
          {props.text}
        </p>
      </Show>
    </li>
  );
}

/** One resolved/pending tool row: spinner while awaiting tool_done, then a
 * check/cross with the host's clean result summary. */
function ToolRow(props: { item: Extract<TimelineItem, { kind: "tool" }> }) {
  const detail = () => props.item.result ?? props.item.summary;
  return (
    <li
      class="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground"
      data-slot="timeline-tool"
    >
      <Switch>
        <Match when={!props.item.done}>
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
      <span class="shrink-0 font-mono text-[0.72rem] text-foreground/90">{props.item.name}</span>
      <Show when={detail()}>
        <span class="min-w-0 truncate">{detail()}</span>
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
  const items = () => buildTimeline(props.events);
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
              <ToolRow item={item as Extract<TimelineItem, { kind: "tool" }>} />
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
 * captured for the turn (it already conveys the tool outcomes, and more). */
export function TurnDetails(props: { events: TimelineEvent[] }) {
  const [expanded, setExpanded] = createSignal(false);
  return (
    <div class="flex flex-col gap-1" data-slot="turn-details">
      <button
        type="button"
        class="inline-flex w-fit items-center gap-1 rounded-full bg-muted px-1.5 py-px text-[0.68rem] text-muted-foreground transition-colors hover:text-foreground"
        aria-expanded={expanded()}
        aria-label="Turn details"
        onClick={() => setExpanded((v) => !v)}
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
