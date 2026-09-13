import { createEffect, createSignal, For, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Timeline } from "../components/chat/Timeline";
import { Activity, CircleAlert, Clock, LoaderCircle, MoreHorizontal, Square } from "../components/ui/icons";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "../components/ui/dropdown-menu";
import { Markdown } from "../components/Markdown";
import { buildTimeline, splitStreamingReply } from "../components/chat/timeline";
import type { ChatMessage } from "../protocol";
import type { TimelineEvent } from "../store/types";
import type { ThreadActivity, ThreadTurn } from "./types";
import { ArtifactCard } from "../artifacts/ArtifactSurface";
import { ThreadLink } from "./ThreadRef";
import { activityText, ownerFacingActivity } from "./conversation";
import { state } from "../store/store";
import { failureReason, mergePersistedToolCalls, toolSummary, workDuration, workLabel } from "./work-summary";
/** A line owned by neither party: centred, muted, one line, between the two
 * columns of the conversation. */
export function ConversationNote(props: { title?: string; children: JSX.Element; expanded?: boolean }) {
  return <div data-slot="conversation-note" title={props.title} class="flex items-center gap-3 text-meta text-muted-foreground">
    <Show when={!props.expanded}><span aria-hidden="true" class="h-px flex-1 bg-border/60" /></Show>
    <div class={props.expanded ? "min-w-0 max-w-full flex-1 rounded-lg border border-border/60 bg-muted/20 px-3 py-2" : "min-w-0 max-w-[80%] truncate text-center"}>{props.children}</div>
    <Show when={!props.expanded}><span aria-hidden="true" class="h-px flex-1 bg-border/60" /></Show>
  </div>;
}
export function ActivityEntry(props: { activity: ThreadActivity }) {
  const data = () => props.activity.data as Record<string, unknown>;
  const report = () => props.activity.kind === "child_report";
  const assignment = () => props.activity.kind === "delegation_received";
  const status = () => String(data().status ?? "");
  /** "completed" is the default outcome and says nothing; a failure does. */
  const notableStatus = () => report() && status() !== "" && status() !== "completed";
  /** A routine note is neither party speaking: one centred muted line between
   * the two columns, never a third bubble competing with them. */
  const note = () => !report() && !assignment() && props.activity.artifact_ids.length === 0 && ownerFacingActivity(props.activity)
    && !activityText(props.activity).includes("\n") && activityText(props.activity).length <= 120;
  return <Show when={!note()} fallback={<span data-activity-id={props.activity.id}><ConversationNote>{activityText(props.activity)} · <time datetime={props.activity.ts}>{new Date(props.activity.ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</time></ConversationNote></span>}><article data-activity-id={props.activity.id} class="space-y-2">
    <Show when={ownerFacingActivity(props.activity)} fallback={<ThreadWork activities={[props.activity]} events={[]} />}>
      {/* Identity and time are what a reader needs; the turn number and a
          "completed" that only restates the default belong in the tooltip. */}
      <p class="flex flex-wrap items-center gap-x-1 text-xs font-medium text-muted-foreground" title={report() ? `Turn ${String(data().child_turn_id)} · ${status()}` : undefined}>
        <Show when={report()} fallback={<Show when={assignment()} fallback="Hirsel">Brief from <ThreadLink id={Number(data().requester_thread_id)} /></Show>}>
          <ThreadLink id={Number(data().child_thread_id)} /><Show when={notableStatus()}><span>· {status()}</span></Show>
        </Show>
        <span>·</span><time datetime={props.activity.ts}>{new Date(props.activity.ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</time>
      </p>
      <Markdown>{activityText(props.activity)}</Markdown>
      <For each={props.activity.artifact_ids}>{id => <ArtifactCard id={id} />}</For>
    </Show>
  </article></Show>;
}
/**
 * What the old "Activity · 11s" header carried, moved to the very end of the
 * card: a live pulse while the turn runs, else the quiet elapsed time, right
 * aligned under everything the turn produced — work rows, prose, artifacts.
 * It is the caller that renders it last, because the prose is the caller's.
 */
export function WorkTail(props: { turn?: ThreadTurn; activities: ThreadActivity[]; events: TimelineEvent[]; message?: ChatMessage }) {
  const [now, setNow] = createSignal(Date.now());
  const running = () => props.turn?.state === "running";
  createEffect(() => running() && state.connection === "connected", active => {
    if (!active) return;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  });
  // A plain reply needs no stopwatch; only a turn that did visible work, or one
  // still running, earns the tail.
  const worked = () => running() || props.events.length > 0 || props.activities.length > 0;
  const duration = () => workDuration(props.turn, now());
  const label = () => workLabel(props.turn, props.events, props.activities, buildTimeline(props.events).filter(item => item.kind === "tool").length, Boolean(props.message));
  return <Show when={props.turn && worked()}>
    <Show when={running()} fallback={<Show when={duration()}>
      <p class="mt-1 text-right text-meta tabular-nums text-muted-foreground/70" data-slot="work-elapsed">{duration()}</p>
    </Show>}>
      <p class="mt-1 flex min-h-5 items-center justify-end" role="status" data-slot="work-live">
        <LoaderCircle class={`size-3.5 ${state.connection === "connected" ? "animate-spin motion-reduce:animate-none" : ""}`} aria-hidden="true" />
        <span class="sr-only">{label()}</span>
      </p>
    </Show>
  </Show>;
}
/** One exact turn/message. No global inspector and no positional association. */
export function ThreadWork(props: { turn?: ThreadTurn; message?: ChatMessage; activities: ThreadActivity[]; events: TimelineEvent[]; live?: boolean }) {
  const [now, setNow] = createSignal(Date.now());
  const [technicalOpen, setTechnicalOpen] = createSignal(false);
  createEffect(() => props.turn?.state === "running" && state.connection === "connected", active => {
    if (!active) return;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  });
  const events = () => splitStreamingReply(props.events).activity;
  const recorded = () => props.message?.tool_calls?.length ? props.message.tool_calls : props.activities.flatMap(activity => { const call = toolSummary(activity); return call ? [call] : []; });
  const resolvedEvents = () => mergePersistedToolCalls(events(), recorded());
  const items = () => buildTimeline(resolvedEvents());
  const toolCount = () => items().filter(item => item.kind === "tool").length;
  const activities = () => props.activities.filter(activity => !ownerFacingActivity(activity) && !toolSummary(activity));
  const hasDetails = () => resolvedEvents().length > 0 || activities().length > 0;
  const failed = () => props.turn?.state === "failed" || (!props.turn && failureReason(props.activities) !== null);
  const stopped = () => props.turn?.state === "cancelled" || props.turn?.state === "interrupted";
  const running = () => props.turn?.state === "running";
  const visible = () => hasDetails() || failed() || stopped() || running() || props.turn?.state === "queued" || (props.turn?.state === "completed" && !props.message);
  /** The card says who is speaking by where it sits and whose avatar is beside
   * it; a "Activity"/"completed" header only repeats that. A label survives
   * only for the states that are NOT the default: queued, stopped, failed. */
  const notable = () => props.turn?.state === "queued" || stopped() || failed();
  const label = () => workLabel(props.turn, resolvedEvents(), props.activities, toolCount(), Boolean(props.message));
  const duration = () => workDuration(props.turn, now());
  /** A turn that is visible but has produced nothing yet draws no box of its
   * own: without content the section keeps no height and no bottom margin, so
   * a freshly started turn is one short line with the live pulse. */
  const blank = () => !notable() && resolvedEvents().length === 0 && !technicalOpen() && !failed() && !stopped();
  const statusIcon = () => <Show when={!failed() && !stopped()} fallback={<Show when={failed()} fallback={<Square class="size-3.5" />}><CircleAlert class="size-3.5 text-destructive" /></Show>}><Show when={props.turn?.state !== "queued"} fallback={<Clock class="size-3.5" />}><Activity class="size-3.5" /></Show></Show>;
  const summary = () => <span class="inline-flex min-w-0 items-center gap-2"><span aria-hidden="true">{statusIcon()}</span><span>{label()}</span><Show when={duration()}><span aria-hidden="true">·</span><span class="shrink-0 tabular-nums">{duration()}</span></Show></span>;
  const technicalId = () => `turn-${props.turn?.id ?? props.activities[0]?.id ?? "activity"}-technical`;
  return <Show when={visible()}><section class={`group relative min-w-0 text-xs text-muted-foreground ${blank() ? "" : "mb-3"}`} data-slot="thread-work" data-turn-id={props.turn?.id} title={duration() ? `Took ${duration()}` : undefined}>
    <Show when={props.turn || props.activities.length > 0 || props.events.length > 0}>
      {/* No header row to hang it from: the menu reveals itself over the card's
          top-right corner on hover or keyboard focus. */}
      <DropdownMenu placement="bottom-end">
        <DropdownMenuTrigger
          class="absolute right-0 top-0 z-10 grid size-11 shrink-0 place-items-center rounded-md text-muted-foreground/70 opacity-0 transition-opacity hover:bg-muted/45 hover:text-foreground focus-visible:opacity-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring group-focus-within:opacity-100 group-hover:opacity-100"
          aria-label="Turn options"
          aria-controls={technicalOpen() ? technicalId() : undefined}
        >
          <MoreHorizontal class="size-4" />
        </DropdownMenuTrigger>
        <DropdownMenuContent>
          <DropdownMenuItem onSelect={() => setTechnicalOpen(value => !value)}>
            {technicalOpen() ? "Hide technical details" : "Technical details"}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </Show>
    <Show when={notable()}>
      <div class="flex min-h-11 min-w-0 items-center gap-2" role={props.turn?.state === "queued" ? "status" : undefined}>
        {summary()}
      </div>
    </Show>
    <Show when={resolvedEvents().length > 0}>
      <Timeline events={resolvedEvents()} live={props.live} settled={Boolean(props.turn && !["queued", "running"].includes(props.turn.state))} />
    </Show>
    <Show when={technicalOpen()}>
      <div id={technicalId()} role="region" aria-label="Technical details" data-slot="work-diagnostics" class="mb-2 ml-4 border-l border-border/60 pl-3 text-meta">
        <Show when={props.turn}>{turn => <p>Turn {turn().id} · {turn().state}</p>}</Show>
        <Show when={props.events.length > 0}>
          <pre class="mt-2 max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-md bg-muted/25 p-2 font-mono">{JSON.stringify(props.events.map(row => ({ seq: row.seq, event: row.event })), null, 2)}</pre>
        </Show>
        <For each={activities()}>{activity => <div class="mt-2"><p>{activity.kind.replaceAll("_", " ")} · <time datetime={activity.ts}>{new Date(activity.ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</time></p><pre class="mt-1 max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-md bg-muted/25 p-2 font-mono">{JSON.stringify(activity.data, null, 2)}</pre></div>}</For>
      </div>
    </Show>
    <Show when={failed()}><div class="max-w-prose space-y-1 pb-2 text-sm">
      <p class="break-words text-destructive" data-slot="work-failure">{failureReason(props.activities) ?? "This run ended before it could finish."}</p>
      <p class="text-muted-foreground" data-slot="work-recovery">Send a message to continue.</p>
    </div></Show>
    <Show when={stopped()}><p class="pb-2">Your conversation is kept. Send a message to continue.</p></Show>
  </section></Show>;
}
