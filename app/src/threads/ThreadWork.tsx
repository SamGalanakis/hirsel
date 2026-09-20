import { For, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Markdown } from "../components/Markdown";
import { ArtifactCard } from "../artifacts/ArtifactSurface";
import { ThreadLink } from "./ThreadRef";
import { RunCard } from "./RunCard";
import { activityText, ownerFacingActivity } from "./conversation";
import type { ThreadActivity } from "./types";
/** A line owned by neither party: centred, muted, one line, between the two
 * columns of the conversation. */
export function ConversationNote(props: { title?: string; children: JSX.Element; expanded?: boolean }) {
  /* The words take their natural width and the two rules share what is left:
     as three equal flex-1 siblings the text got a third of the measure and
     truncated at 1440px with room to spare on either side. */
  return <div data-slot="conversation-note" title={props.title} class="flex items-center gap-3 text-meta text-muted-foreground">
    <Show when={!props.expanded}><span aria-hidden="true" class="h-px min-w-4 flex-1 bg-border/60" /></Show>
    <div class={props.expanded ? "min-w-0 max-w-full flex-1 rounded-lg border border-border/60 bg-muted/20 px-3 py-2" : "min-w-0 max-w-full shrink truncate text-center"}>{props.children}</div>
    <Show when={!props.expanded}><span aria-hidden="true" class="h-px min-w-4 flex-1 bg-border/60" /></Show>
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
  return <Show when={!note()} fallback={<span data-activity-id={props.activity.id}><ConversationNote><span>{activityText(props.activity)}</span> · <time datetime={props.activity.ts}>{new Date(props.activity.ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</time></ConversationNote></span>}><article data-activity-id={props.activity.id} class="space-y-2">
    <Show when={ownerFacingActivity(props.activity)} fallback={<RunCard activities={[props.activity]} events={[]} />}>
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
