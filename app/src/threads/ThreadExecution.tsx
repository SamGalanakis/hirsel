import { For, Show } from "solid-js";
import { Timeline, TurnDetails } from "../components/chat/Timeline";
import { CommittedToolCalls } from "../components/chat/ToolCalls";
import { Markdown } from "../components/Markdown";
import type { TimelineEvent } from "../store/types";
import type { ThreadActivity } from "./types";
import { threadState } from "./store";
export function activityText(activity: ThreadActivity): string {
  if (!activity.data || typeof activity.data !== "object") return "";
  const data = activity.data as Record<string, unknown>;
  const payload = data.payload && typeof data.payload === "object" ? data.payload as Record<string, unknown> : data;
  return [payload.description, payload.message, payload.text, payload.content_md, payload.summary]
    .filter((value, index, values): value is string => typeof value === "string" && value.length > 0 && values.indexOf(value) === index).join("\n\n");
}

/** These are explicit owner-facing results emitted by the fork and process tools. */
export function ownerFacingActivity(activity: ThreadActivity): boolean {
  return ["info", "summary", "plugin.info", "plugin.summary", "process_completed"].includes(activity.kind) && activityText(activity).length > 0;
}

export function ThreadExecution(props: { id: number; liveEvents: TimelineEvent[] }) {
  const history = () => threadState.histories[props.id];
  const messages = () => history()?.messages.filter(message => threadState.turnDetails[message.id]?.length || message.tool_calls?.length) ?? [];
  const activities = () => history()?.activities.filter(activity => !ownerFacingActivity(activity)) ?? [];
  const hasDetails = () => messages().length > 0 || activities().length > 0 || props.liveEvents.length > 0 || (history()?.turns.length ?? 0) > 0;
  return <Show when={hasDetails()}><details class="border-t border-border/60 pt-3 text-sm text-muted-foreground" data-slot="execution-inspector">
    <summary class="min-h-11 cursor-pointer rounded-md py-3 font-medium hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">Inspect execution</summary>
    <div class="space-y-4 pb-3">
      <For each={history()?.turns}>{turn => <p class="text-xs">Turn {turn.id} · {turn.state}</p>}</For>
      <For each={messages()}>{message => <div><Show when={threadState.turnDetails[message.id]?.length} fallback={<CommittedToolCalls toolCalls={message.tool_calls ?? []} />}><TurnDetails events={threadState.turnDetails[message.id] ?? []} /></Show></div>}</For>
      <Show when={props.liveEvents.length > 0}><Timeline events={props.liveEvents} live /></Show>
      <ol class="space-y-3"><For each={activities()}>{activity => <li><p class="mb-1 text-xs"><time datetime={activity.ts}>{new Date(activity.ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</time> · {activity.kind.replaceAll("_", " ")}</p><Show when={activityText(activity)}>{text => <Markdown>{text()}</Markdown>}</Show></li>}</For></ol>
    </div>
  </details></Show>;
}
