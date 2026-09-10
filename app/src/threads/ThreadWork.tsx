import { createEffect, createSignal, For, Show } from "solid-js";
import { Timeline } from "../components/chat/Timeline";
import { Check, CircleAlert, Clock, LoaderCircle, Square } from "../components/ui/icons";
import { Markdown } from "../components/Markdown";
import { buildTimeline, splitStreamingReply } from "../components/chat/timeline";
import type { ChatMessage } from "../protocol";
import type { TimelineEvent } from "../store/types";
import type { ThreadActivity, ThreadTurn } from "./types";
import { ArtifactCard } from "../artifacts/ArtifactSurface";
import { ThreadLink } from "./ThreadRef";
import { activityText, ownerFacingActivity } from "./conversation";
import { state } from "../store/store";
import { activityData, failureReason, mergePersistedToolCalls, remainingTools, toolSummary, workDuration, workLabel } from "./work-summary";
export function ActivityEntry(props: { activity: ThreadActivity }) {
  const data = () => props.activity.data as Record<string, unknown>;
  const report = () => props.activity.kind === "child_report";
  const assignment = () => props.activity.kind === "delegation_received";
  return <article data-activity-id={props.activity.id} class="space-y-2">
    <Show when={ownerFacingActivity(props.activity)} fallback={<ThreadWork activities={[props.activity]} events={[]} />}>
      <p class="flex flex-wrap items-center gap-x-1 text-xs font-medium text-muted-foreground">
        <Show when={report()} fallback={<Show when={assignment()} fallback="Hirsel">Brief from <ThreadLink id={Number(data().requester_thread_id)} /></Show>}>
          <ThreadLink id={Number(data().child_thread_id)} /> · <span>{String(data().status)}</span><span>· Turn {String(data().child_turn_id)}</span>
        </Show>
        <span>·</span><time datetime={props.activity.ts}>{new Date(props.activity.ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</time>
      </p>
      <Markdown>{activityText(props.activity)}</Markdown>
      <For each={props.activity.artifact_ids}>{id => <ArtifactCard id={id} />}</For>
    </Show>
  </article>;
}
/** One exact turn/message. No global inspector and no positional association. */
export function ThreadWork(props: { turn?: ThreadTurn; message?: ChatMessage; activities: ThreadActivity[]; events: TimelineEvent[]; live?: boolean }) {
  const [now, setNow] = createSignal(Date.now());
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
  const extraTools = () => remainingTools(recorded(), items());
  const toolCount = () => items().filter(item => item.kind === "tool").length + extraTools().length;
  const activities = () => props.activities.filter(activity => !ownerFacingActivity(activity) && !toolSummary(activity));
  const hasDetails = () => resolvedEvents().length > 0 || extraTools().length > 0 || activities().length > 0;
  const failed = () => props.turn?.state === "failed" || (!props.turn && failureReason(props.activities) !== null);
  const stopped = () => props.turn?.state === "cancelled" || props.turn?.state === "interrupted";
  const running = () => props.turn?.state === "running";
  const visible = () => hasDetails() || failed() || stopped() || running() || props.turn?.state === "queued" || (props.turn?.state === "completed" && !props.message);
  const label = () => workLabel(props.turn, resolvedEvents(), props.activities, toolCount(), Boolean(props.message));
  const duration = () => workDuration(props.turn, now());
  const statusIcon = () => <Show when={!failed() && !stopped()} fallback={<Show when={failed()} fallback={<Square class="size-3.5" />}><CircleAlert class="size-3.5 text-destructive" /></Show>}><Show when={!running()} fallback={<LoaderCircle class={`size-3.5 ${state.connection === "connected" ? "animate-spin motion-reduce:animate-none" : ""}`} />}><Show when={props.turn?.state !== "queued"} fallback={<Clock class="size-3.5" />}><Check class="size-3.5" /></Show></Show></Show>;
  const summary = () => <span class="inline-flex min-w-0 items-center gap-2"><span aria-hidden="true">{statusIcon()}</span><span>{label()}</span><Show when={duration()}><span aria-hidden="true">·</span><span class="shrink-0 tabular-nums">{duration()}</span></Show></span>;
  return <Show when={visible()}><section class="mt-2 min-w-0 text-xs text-muted-foreground" data-slot="thread-work" data-turn-id={props.turn?.id}>
    <Show when={hasDetails()} fallback={<p class="py-2" role={running() || props.turn?.state === "queued" ? "status" : undefined}>{summary()}</p>}>
      <details data-slot="work-details">
        <summary class="min-h-11 w-fit max-w-full cursor-pointer rounded-md py-3 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">{summary()}</summary>
        <div class="space-y-3 pb-3 pl-5">
          <Show when={resolvedEvents().length > 0}><Timeline events={resolvedEvents()} live={props.live} settled={Boolean(props.turn && !["queued", "running"].includes(props.turn.state))} /></Show>
          <Show when={extraTools().length > 0}><ul class="space-y-2" aria-label="Recorded tools"><For each={extraTools()}>{call => <li class="flex items-center gap-2"><Show when={call.ok} fallback={<CircleAlert class="size-3.5 shrink-0 text-destructive" aria-label="failed" />}><Check class="size-3.5 shrink-0" aria-label="ok" /></Show><span class="break-words">{call.name.replaceAll("_", " ").replaceAll(".", " · ")}</span></li>}</For></ul></Show>
          <For each={activities().filter(activity => activity.kind === "execution_progress")}>{activity => <p class="break-words">{String(activityData(activity).summary ?? "")}</p>}</For>
          <details data-slot="work-diagnostics"><summary class="min-h-11 w-fit cursor-pointer rounded-md py-3 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">Diagnostics</summary>
            <Show when={props.turn}>{turn => <p>Turn {turn().id} · {turn().state}</p>}</Show>
            <For each={activities()}>{activity => <div class="mt-2"><p>{activity.kind.replaceAll("_", " ")} · <time datetime={activity.ts}>{new Date(activity.ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</time></p><pre class="mt-1 max-h-64 overflow-auto whitespace-pre-wrap break-words rounded bg-muted/40 p-2">{JSON.stringify(activity.data, null, 2)}</pre></div>}</For>
          </details>
        </div>
      </details>
    </Show>
    <Show when={failed()}><p class="max-w-prose break-words pb-2 text-sm text-destructive" data-slot="work-failure">{failureReason(props.activities) ?? "This run ended before it could finish."} <span class="text-muted-foreground">Send a message to continue.</span></p></Show>
    <Show when={stopped()}><p class="pb-2">Your conversation is kept. Send a message to continue.</p></Show>
  </section></Show>;
}
