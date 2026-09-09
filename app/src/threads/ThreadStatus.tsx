import { Show } from "solid-js";
import { Check, CircleAlert, Clock, LoaderCircle, Square } from "../components/ui/icons";
import { state } from "../store/store";
import { threadStatus } from "./status";
import type { Thread } from "./types";
export function ThreadStatus(props: { thread: Thread; now: number }) {
  const status = () => threadStatus(props.thread, props.now, state.connection === "connected");
  return <span class="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground">
    <Show when={props.thread.attention === "needs_owner"}><span class="inline-flex items-center gap-1 text-status-attention"><CircleAlert class="size-3.5" /><span>Needs you</span></span></Show>
    <span class={["inline-flex items-center gap-1 tabular-nums", { "text-status-active": status().state === "running", "text-primary": status().state === "completed", "text-destructive": status().state === "failed" }]} title={status().timestamp ? `${status().state === "running" ? "Started" : status().timeLabel}: ${new Date(status().timestamp!).toLocaleString()}` : undefined}>
      <Show when={status().state === "running"}><LoaderCircle class={`size-3.5 ${status().stale ? "" : "animate-spin motion-reduce:animate-none"}`} /></Show>
      <Show when={status().state === "queued"}><Clock class="size-3.5" /></Show>
      <Show when={status().state === "completed"}><Check class="size-3.5" /></Show>
      <Show when={status().state === "failed" || status().state === "interrupted"}><CircleAlert class="size-3.5" /></Show>
      <Show when={status().state === "cancelled"}><Square class="size-3.5" /></Show>
      <span>{status().label}</span><Show when={status().age}><span aria-hidden="true">·</span><time datetime={status().timestamp!} aria-label={`${status().timeLabel} ${status().age} ago`}>{status().age}</time></Show>
    </span>
    <Show when={props.thread.settled_at}><span>Settled</span></Show>
    <Show when={props.thread.archived_at}><span>Archived</span></Show>
    <Show when={!props.thread.archived_at && props.thread.snoozed_until && Date.parse(props.thread.snoozed_until) > props.now}><span title={`Snoozed until ${new Date(props.thread.snoozed_until!).toLocaleString()}`}>Snoozed</span></Show>
  </span>;
}
