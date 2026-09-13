import { createEffect, createMemo, createSignal, For, Match, Show, Switch } from "solid-js";
import { ArtifactCard } from "../artifacts/ArtifactSurface";
import { Timeline } from "../components/chat/Timeline";
import { buildTimeline, splitStreamingReply } from "../components/chat/timeline";
import { Markdown } from "../components/Markdown";
import { Check, ChevronRight, CircleAlert, LoaderCircle, Square } from "../components/ui/icons";
import type { ChatMessage } from "../protocol";
import { state } from "../store/store";
import type { TimelineEvent } from "../store/types";
import { ownerFacingActivity } from "./conversation";
import { executionLabel } from "./execution-label";
import { hasTrace, runOrigin, runOriginLabel, runOutcome, runOutcomeLabel, turnArtifactIds, type RunOutcome } from "./run-card";
import { setTurnExpanded, threadState } from "./store";
import type { Thread, ThreadActivity, ThreadTurn } from "./types";
import { failureReason, mergePersistedToolCalls, toolSummary, workDuration, workLabel } from "./work-summary";

/** Everything the run recorded while it was working: the ordered step rows the
 * live view already draws, the activity the Host kept that is not a step, and
 * the raw turn data underneath both. It is one region, disclosed as a whole. */
function TurnTrace(props: { ref?: (node: HTMLDivElement) => void; turn?: ThreadTurn; events: TimelineEvent[]; raw: TimelineEvent[]; activities: ThreadActivity[]; live?: boolean; id: string }) {
  const settled = () => Boolean(props.turn && !["queued", "running"].includes(props.turn.state));
  const diagnostics = () => props.activities.filter(activity => !ownerFacingActivity(activity) && !toolSummary(activity));
  return <div ref={node => props.ref?.(node)} id={props.id} data-slot="run-card-trace" class="mb-2 min-w-0 text-xs text-muted-foreground">
    <Show when={props.events.length > 0}>
      <Timeline events={props.events} live={props.live} settled={settled()} />
    </Show>
    <Show when={props.events.length === 0 && diagnostics().length === 0}>
      <p class="ml-1 border-l border-border/60 pl-3 text-meta">No steps were recorded for this run.</p>
    </Show>
    <details data-slot="run-card-technical" class="ml-1 mt-1 border-l border-border/60 pl-3 text-meta">
      <summary class="min-h-8 cursor-pointer py-1 pointer-coarse:min-h-11">Technical details</summary>
      <div role="region" aria-label="Technical details" data-slot="work-diagnostics">
        <Show when={props.turn}>{turn => <p>Turn {turn().id} · {turn().state}</p>}</Show>
        <Show when={props.raw.length > 0}>
          <pre class="mt-2 max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-md bg-muted/25 p-2 font-mono">{JSON.stringify(props.raw.map(row => ({ seq: row.seq, event: row.event })), null, 2)}</pre>
        </Show>
        <For each={diagnostics()}>{activity => <div class="mt-2"><p>{activity.kind.replaceAll("_", " ")} · <time datetime={activity.ts}>{new Date(activity.ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</time></p><pre class="mt-1 max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-md bg-muted/25 p-2 font-mono">{JSON.stringify(activity.data, null, 2)}</pre></div>}</For>
      </div>
    </details>
  </div>;
}

/** The one mark an outcome gets. Text carries the word beside it, so the glyph
 * only has to separate finished from failed from stopped at a glance. */
function OutcomeMark(props: { outcome: RunOutcome }) {
  return <Switch>
    <Match when={props.outcome === "running"}><LoaderCircle class={`size-3.5 shrink-0 text-status-active ${state.connection === "connected" ? "animate-spin motion-reduce:animate-none" : ""}`} aria-hidden="true" /></Match>
    <Match when={props.outcome === "failed"}><CircleAlert class="size-3.5 shrink-0 text-destructive" aria-hidden="true" /></Match>
    <Match when={props.outcome === "done"}><Check class="size-3.5 shrink-0 text-status-success" aria-hidden="true" /></Match>
    <Match when={true}><Square class="size-3.5 shrink-0" aria-hidden="true" /></Match>
  </Switch>;
}

/**
 * One finished — or running — agent turn, as one card.
 *
 * The header names the run: what started it, where it ran, how long it took and
 * how it ended. Under it sits the execution trace, collapsed once the run is
 * over and open while it is live, so watching a turn work and re-opening it
 * afterwards are the same component in two states rather than two surfaces.
 * The reply, and whatever the run published, follow underneath, where they stay
 * readable without opening anything.
 */
export function RunCard(props: { turn?: ThreadTurn; message?: ChatMessage; trigger?: ChatMessage; thread?: Thread; activities: ThreadActivity[]; events: TimelineEvent[]; live?: boolean }) {
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
  const executor = () => executionLabel(props.thread?.execution);
  const duration = () => workDuration(props.turn, now());
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
  let trace: HTMLDivElement | undefined;
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
      {/* Dense one-line identity: what asked for the run, where it ran, how long
          it took, how it ended — and the one control that opens its trace. */}
      <button
        type="button"
        ref={node => { header = node; }}
        data-slot="run-card-header"
        aria-expanded={expanded() ? "true" : "false"}
        aria-controls={expanded() ? traceId() : undefined}
        class="-mx-1 mb-1.5 flex min-h-8 w-full min-w-0 items-center gap-1.5 rounded-md px-1 text-left text-meta text-muted-foreground transition-colors hover:bg-muted/45 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:min-h-11"
        onClick={() => props.turn && setTurnExpanded(props.turn.id, !expanded())}
      >
        <ChevronRight class={`size-3 shrink-0 transition-transform ${expanded() ? "rotate-90" : ""}`} aria-hidden="true" />
        <span class="shrink-0">{runOriginLabel(origin())}</span>
        <span aria-hidden="true">·</span>
        <span class={`min-w-0 truncate font-mono ${executor().muted ? "text-muted-foreground/70" : ""}`} data-slot="run-card-executor">{executor().text}</span>
        <Show when={duration()}><span aria-hidden="true">·</span><span class="shrink-0 tabular-nums">{duration()}</span></Show>
        <span class="ml-auto inline-flex shrink-0 items-center gap-1" data-slot="run-card-outcome">
          <OutcomeMark outcome={outcome()} />
          <span>{runOutcomeLabel(outcome())}</span>
        </span>
      </button>
      <Show when={running()}><p class="sr-only" role="status">{label()}</p></Show>
    </Show>
    <Show when={expanded()}>
      <TurnTrace ref={node => { trace = node; }} turn={props.turn} events={events()} raw={props.events} activities={props.activities} live={props.live} id={traceId()} />
    </Show>
    <Show when={body()}><Markdown>{body()}</Markdown></Show>
    <For each={artifacts()}>{id => <ArtifactCard id={id} />}</For>
    <Show when={failed()}><div class="max-w-prose space-y-1 pt-1 text-sm">
      <p class="break-words text-destructive" data-slot="work-failure">{failureReason(props.activities) ?? "This run ended before it could finish."}</p>
      <p class="text-muted-foreground" data-slot="work-recovery">Send a message to continue.</p>
    </div></Show>
    <Show when={stopped()}><p class="pt-1 text-xs text-muted-foreground">Your conversation is kept. Send a message to continue.</p></Show>
  </section>;
}
