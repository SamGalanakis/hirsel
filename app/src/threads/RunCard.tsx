import { createEffect, createMemo, createSignal, For, Match, Show, Switch } from "solid-js";
import { ArtifactCard } from "../artifacts/ArtifactSurface";
import { Timeline } from "../components/chat/Timeline";
import { buildTimeline, splitStreamingReply } from "../components/chat/timeline";
import { Markdown } from "../components/Markdown";
import { CubeSpinner } from "../components/CubeSpinner";
import { Check, ChevronRight, CircleAlert, Square } from "../components/ui/icons";
import { SectionLabel } from "../components/ui/section-label";
import type { ChatMessage } from "../protocol";
import { state } from "../store/store";
import type { TimelineEvent } from "../store/types";
import { ownerFacingActivity } from "./conversation";
import { hasTrace, runOrigin, runOriginLabel, runOutcome, runOutcomeLabel, turnArtifactIds, type RunOutcome } from "./run-card";
import { setTurnExpanded, threadState } from "./store";
import type { ThreadActivity, ThreadTurn } from "./types";
import { failureReason, mergePersistedToolCalls, toolSummary, workDuration, workLabel } from "./work-summary";

/** Everything the run recorded while it was working: the ordered step rows the
 * live view already draws, and the activity the Host kept that is not a step.
 * Both are ordinary rows of the one trace — there is no second, technical
 * surface hidden under a disclosure, and no raw protocol dump: an event log
 * nobody outside this file can read was never the Owner's to fold away. */
function TurnTrace(props: { ref?: (node: HTMLElement) => void; turn?: ThreadTurn; events: TimelineEvent[]; activities: ThreadActivity[]; live?: boolean; id: string }) {
  const settled = () => Boolean(props.turn && !["queued", "running"].includes(props.turn.state));
  const records = () => props.activities.filter(activity => !ownerFacingActivity(activity) && !toolSummary(activity));
  /* The trace is a CONTAINED block, not loose text under the header: a quiet
     fill and a hairline give the run's steps an edge, and one small label says
     what the block is. Opened, it used to read as unlabelled dim italics
     floating between the header and the reply, so nothing told the Owner where
     the machine's account of the run started or stopped. */
  /* The block has a height budget. A long run's reasoning and tool output used
     to unroll the whole trace down the conversation, so opening one card pushed
     the reply it belonged to off the screen; now the trace scrolls inside a
     40dvh window with the app's edge fade, and the JSON records keep their own
     smaller cap inside it. */
  return <section ref={node => props.ref?.(node)} id={props.id} data-slot="run-card-trace" class="scroll-fade-y mb-2 max-h-[40dvh] min-w-0 overflow-y-auto rounded-lg border border-border/60 bg-muted/20 px-2.5 py-2 text-xs text-muted-foreground" aria-label="Run steps">
    <SectionLabel class="mb-1.5">Steps</SectionLabel>
    <Show when={props.events.length > 0}>
      <Timeline events={props.events} live={props.live} settled={settled()} />
    </Show>
    <Show when={props.events.length === 0 && records().length === 0}>
      <p class="ml-1 border-l border-border/60 pl-3 text-meta">No steps were recorded for this run.</p>
    </Show>
    <For each={records()}>{activity => <div data-slot="run-card-record" class="ml-1 mt-1 border-l border-border/60 pl-3 text-meta">
      <p>{activity.kind.replaceAll("_", " ")} · <time datetime={activity.ts}>{new Date(activity.ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</time></p>
      <pre class="mt-1 max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-md bg-muted/25 p-2 font-mono">{JSON.stringify(activity.data, null, 2)}</pre>
    </div>}</For>
  </section>;
}

/** The one mark an outcome gets. Text carries the word beside it, so the glyph
 * only has to separate finished from failed from stopped at a glance. */
function OutcomeMark(props: { outcome: RunOutcome }) {
  return <Switch>
    <Match when={props.outcome === "running"}><CubeSpinner paused={state.connection !== "connected"} /></Match>
    <Match when={props.outcome === "failed"}><CircleAlert class="size-3.5 shrink-0 text-destructive" aria-hidden="true" /></Match>
    <Match when={props.outcome === "done"}><Check class="size-3.5 shrink-0 text-status-success" aria-hidden="true" /></Match>
    <Match when={true}><Square class="size-3.5 shrink-0" aria-hidden="true" /></Match>
  </Switch>;
}

/**
 * One finished — or running — agent turn, as one card.
 *
 * The header names the run in one quiet line: what started it when that is not
 * the Owner asking, how long it took and how it ended. Under it sits the
 * execution trace, collapsed once the run is over and open while it is live, so watching a turn work and re-opening it
 * afterwards are the same component in two states rather than two surfaces.
 * The reply, and whatever the run published, follow underneath, where they stay
 * readable without opening anything.
 */
export function RunCard(props: { turn?: ThreadTurn; message?: ChatMessage; trigger?: ChatMessage; activities: ThreadActivity[]; events: TimelineEvent[]; live?: boolean }) {
  const [now, setNow] = createSignal(Date.now());
  const running = () => props.turn?.state === "running";
  createEffect(() => running() && state.connection === "connected", active => {
    if (!active) return;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  });
  const split = createMemo(() => splitStreamingReply(props.events));
  const recorded = () => props.message?.tool_calls?.length ? props.message.tool_calls : props.activities.flatMap(activity => { const call = toolSummary(activity); return call ? [call] : []; });
  const events = createMemo(() => mergePersistedToolCalls(split().activity, recorded()));
  const body = () => props.message?.body ?? split().reply;
  const outcome = () => runOutcome(props.turn, props.message, props.events);
  const origin = () => runOrigin(props.turn, props.trigger, props.activities);
  const duration = () => workDuration(props.turn, now());
  const originLabel = () => runOriginLabel(origin());
  /** A completed run that replied has the reply as its evidence; the word
   * "done" over it only repeats what the mark already says. */
  const outcomeWord = () => outcome() === "done" || outcome() === "running" ? null : runOutcomeLabel(outcome());
  const artifacts = () => turnArtifactIds(props.message, props.activities);
  /** A live run is open because the Owner is watching it happen; a finished one
   * is closed because its reply is the answer. Either way the Owner's own last
   * choice for this exact turn wins for the rest of the session. Recorded work
   * with no turn to own it has no header to open, so it stays visible. */
  const expanded = () => props.turn ? threadState.expandedTurns[props.turn.id] ?? running() : hasTrace(events(), props.activities);
  const traceId = () => `run-${props.turn?.id ?? "none"}-trace`;
  const failed = () => props.turn?.state === "failed" || (!props.turn && failureReason(props.activities) !== null);
  const stopped = () => props.turn?.state === "cancelled" || props.turn?.state === "interrupted";
  let header: HTMLButtonElement | undefined;
  let trace: HTMLElement | undefined;
  let focusInTrace = false;
  /** A run that settles folds its trace away. Whoever was reading a step inside
   * it keeps a place to stand: focus lands on the header that now owns it,
   * never on the document. */
  let wasExpanded = false;
  createEffect(expanded, open => {
    const folded = wasExpanded && !open;
    wasExpanded = open;
    if (!folded || !focusInTrace) return;
    focusInTrace = false;
    header?.focus({ preventScroll: true });
  });
  const label = () => workLabel(props.turn, events(), props.activities, buildTimeline(events()).filter(item => item.kind === "tool").length, Boolean(props.message));
  return <section class="min-w-0" data-slot="run-card" onFocusIn={event => { focusInTrace = trace !== undefined && event.target instanceof Node && trace.contains(event.target); }} data-turn-id={props.turn?.id} data-outcome={props.turn ? outcome() : undefined}>
    <Show when={props.turn}>
      {/* Dense one-line identity: what started the run when that is worth
          saying, how long it took, how it ended — and the one control that
          opens its trace. The spoken name keeps the outcome word the quiet
          line drops, so a header showing only a mark still announces itself. */}
      <button
        type="button"
        ref={node => { header = node; }}
        data-slot="run-card-header"
        aria-label={[runOutcomeLabel(outcome()), originLabel(), duration()].filter(Boolean).join(" · ")}
        aria-expanded={expanded() ? "true" : "false"}
        aria-controls={expanded() ? traceId() : undefined}
        class="-mx-1 mb-1.5 flex min-h-6 w-full min-w-0 items-center gap-1.5 rounded-md px-1 py-0.5 text-left text-meta text-muted-foreground tabular-nums transition-colors hover:bg-accent/40 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring pointer-coarse:min-h-11"
        onClick={() => props.turn && setTurnExpanded(props.turn.id, !expanded())}
      >
        <ChevronRight class={`size-3 shrink-0 transition-transform ${expanded() ? "rotate-90" : ""}`} aria-hidden="true" />
        {/* One fixed slot for the run's state, always the same 14px square: the
            tumbling cube while it works, the outcome mark once it is over. The
            elapsed time sits immediately after it and never moves, so a run
            settling does not shuffle the line the Owner is reading. */}
        <span class="inline-flex size-3.5 shrink-0 items-center justify-center" data-slot="run-card-outcome"><OutcomeMark outcome={outcome()} /></span>
        <Show when={duration()}>
          <span class="shrink-0 tabular-nums">{duration()}</span>
        </Show>
        {/* Everything that is not the live state trails at the far edge, and a
            running turn says none of it: chevron, cube, elapsed, nothing else. */}
        <span class="ml-auto inline-flex min-w-0 items-center gap-1.5">
          <Show when={!running() && originLabel()}>{label => <span class="min-w-0 truncate" data-slot="run-card-origin">{label()}</span>}</Show>
          <Show when={outcomeWord()}>{word => <span class="shrink-0">{word()}</span>}</Show>
        </span>
      </button>
      <Show when={running()}><p class="sr-only" role="status">{label()}</p></Show>
    </Show>
    <Show when={expanded()}>
      <TurnTrace ref={node => { trace = node; }} turn={props.turn} events={events()} activities={props.activities} live={props.live} id={traceId()} />
    </Show>
    <Show when={body()}><Markdown>{body()}</Markdown></Show>
    <For each={artifacts()}>{id => <ArtifactCard id={id} />}</For>
    <Show when={failed()}><div class="max-w-prose space-y-1 pt-1 text-sm">
      <p class="break-words text-destructive" data-slot="work-failure">{failureReason(props.activities) ?? "This run ended before it could finish."}</p>
      <p class="text-meta text-muted-foreground" data-slot="work-recovery">Send a message to continue.</p>
    </div></Show>
    <Show when={stopped()}><p class="pt-1 text-meta text-muted-foreground">Your conversation is kept. Send a message to continue.</p></Show>
  </section>;
}
