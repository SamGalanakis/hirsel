import { For, onCleanup, Show } from "solid-js";
import { Markdown } from "../components/Markdown";
import { CubeSpinner } from "../components/CubeSpinner";
import { Clock, MessagesSquare } from "../components/ui/icons";
import { ArtifactCard } from "../artifacts/ArtifactSurface";
import { getClient } from "../ws/client";
import { buildTimeline, splitStreamingReply } from "../components/chat/timeline";
import { ActivityEntry, ConversationNote } from "./ThreadWork";
import { RunCard } from "./RunCard";
import type { ChatMessage, ProcessOrigin, TriggerLabel } from "../protocol";
import type { ConversationEntry } from "./conversation";
import type { ThreadHistory } from "./model";
import { threadState } from "./store";
import { workLabel } from "./work-summary";
import { state } from "../store/store";
import { anyOverlayOpen } from "../lib/focus";

/** A keyed response moves to its final chronological position without replacing
 * controls. Browsers can still drop focus when moving that existing DOM node. */
function preserveMovedFocus(article: HTMLElement): () => void {
  let focused: HTMLElement | null = null;
  const observer = new MutationObserver(() => {
    if (focused?.isConnected && document.activeElement === document.body && !anyOverlayOpen()) focused.focus({ preventScroll: true });
  });
  const enter = (event: FocusEvent) => {
    if (!(event.target instanceof HTMLElement)) return;
    focused = event.target;
    observer.observe(article.parentElement ?? article, { childList: true, subtree: true });
  };
  const leave = (event: FocusEvent) => {
    // An intentional focus move wins. Detached-node blur is the one exception.
    if (event.relatedTarget || focused?.isConnected) { focused = null; observer.disconnect(); }
  };
  article.addEventListener("focusin", enter); article.addEventListener("focusout", leave);
  return () => { observer.disconnect(); article.removeEventListener("focusin", enter); article.removeEventListener("focusout", leave); };
}
/** The parent keys this component by durable entry key. Accessors keep its
 * contents reactive without replacing open disclosures or focused controls. */
export function ThreadMessage(props: { entry: ConversationEntry; history: ThreadHistory; threadId: number }) {
  let releaseFocus: (() => void) | undefined;
  onCleanup(() => releaseFocus?.());
  const message = () => props.entry.kind === "message" ? props.entry.message : undefined;
  const process = () => message()?.origin?.kind === "process" ? message() : undefined;
  const turn = () => props.entry.kind === "activity" ? undefined : props.entry.turn;
  const owner = () => message()?.author === "owner";
  const activity = () => (props.entry as Extract<ConversationEntry, {kind:"activity"}>).activity;
  const activities = (id: number | undefined) => id === undefined ? [] : props.history.activities.filter(activity => activity.turn_id === id);
  const events = () => turn() === undefined ? [] : threadState.turnDetails[turn()!.id] ?? [];
  /** The exact message that asked for this turn, joined by ID — it is what the
   * card's header names as the run's origin. */
  const trigger = () => { const id = turn()?.owner_message_id; return id === null || id === undefined ? undefined : props.history.messages.find(row => row.id === id); };
  const split = () => splitStreamingReply(events());
  /** A turn that has started but said nothing yet is not a card: an empty box
   * claims the Agent produced something. Until the first reasoning line, pill or
   * word arrives it is one spinner on the margin where the card will open. */
  const pending = () => !owner() && !message() && turn()?.state === "running"
    && activities(turn()?.id).length === 0 && split().reply === "" && buildTimeline(split().activity).length === 0;
  return <>
    <Show when={process()}>{delivery => <ProcessNote message={delivery()} origin={delivery().origin!} />}</Show>
    <Show when={props.entry.kind !== "activity" && !process()}>
      {/* Who is speaking is readable before a word is: the Owner sits right in
          the filled emphasis pair (a near-white fill on the dark theme, the
          accent on the light one) at conversational width, the Agent sits left
          on a neutral surface that hugs its own content, so the two never share an
          edge and the column reads left against right. Alignment carries the
          speaker on its own, so neither side spends a 56px avatar gutter saying
          again what the fill and the edge already said: both sit on the one
          gutter the scroller gives every row. Wide work rows and code scroll
          inside the card, never the page. */}
      <article ref={node => { releaseFocus = preserveMovedFocus(node); }} data-message-id={message()?.id} data-execution-turn={!message() ? turn()?.id : undefined} data-author={owner() ? "owner" : "agent"} aria-label={owner() ? "You" : "Hirsel"} class={["flex", owner() ? "flex-row-reverse" : ""]}>
        <Show when={pending()}>
          <p data-slot="turn-pending" role="status" class="flex min-h-5 items-center text-muted-foreground">
            <CubeSpinner size={16} paused={state.connection !== "connected"} />
            <span class="sr-only">{workLabel(turn(), events(), activities(turn()?.id), 0, false)}</span>
          </p>
        </Show>
        <Show when={!pending()}>
        <div data-slot={owner() ? "owner-message" : "agent-message"} class={owner() ? "min-w-0 max-w-reply rounded-xl rounded-br-sm bg-primary px-3.5 py-2.5 text-primary-foreground [&_code]:border-current/15 [&_code]:bg-current/10" : "min-w-0 flex-1 rounded-xl rounded-bl-sm border border-border/60 bg-surface px-3.5 py-2.5"}>
          {/* The Agent side is one run card: what started the turn, where it
              ran, its trace, its reply and whatever it published. The Owner
              side is the message itself, which is all there is to say. */}
          <Show when={!owner()} fallback={<><Markdown>{message()?.body ?? ""}</Markdown><For each={message()?.artifact_ids ?? []}>{id => <ArtifactCard id={id} />}</For></>}>
            <RunCard turn={turn()} message={message()} trigger={trigger()} activities={activities(turn()?.id)} events={events()} live={turn()?.state === "running"} />
          </Show>
          <Show when={message()?.attachments?.length}><ul class="mt-2 text-xs text-muted-foreground"><For each={message()?.attachments}>{blob => <li><button class="underline" onClick={() => { void getClient()?.getBlobUrl(blob.id).then(url => window.open(url, "_blank", "noopener,noreferrer")); }}>{blob.name}</button></li>}</For></ul></Show>
        </div>
        </Show>
      </article>
    </Show>
    <Show when={props.entry.kind === "activity"}><ActivityEntry activity={activity()} /></Show>
  </>;
}

function triggerText(trigger: TriggerLabel): string {
  switch (trigger.kind) {
    case "timer": return `timer · ${trigger.in_secs !== undefined ? `in ${trigger.in_secs}s` : trigger.every_secs !== undefined ? `every ${trigger.every_secs}s` : trigger.at !== undefined ? `at ${trigger.at}` : trigger.label}`;
    case "cron": return `cron · ${trigger.expr}${trigger.tz ? ` (${trigger.tz})` : ""}`;
    case "thread": return `${trigger.event} · #${trigger.thread_id} ${trigger.title}`;
    case "other": return trigger.key;
  }
}
function ProcessNote(props: { message: ChatMessage; origin: ProcessOrigin }) {
  const json = () => props.origin.result !== null && typeof props.origin.result === "object" && !props.origin.error;
  return <article data-message-id={props.message.id} data-slot="process-message" aria-label="Process delivery">
    <ConversationNote expanded>
      <div class="flex min-w-0 items-center gap-1.5 text-meta text-muted-foreground">
        <span aria-hidden="true"><Show when={props.origin.trigger.kind === "thread"} fallback={<Clock class="size-3.5 shrink-0" />}><MessagesSquare class="size-3.5 shrink-0" /></Show></span>
        <code class="max-w-[40%] shrink-0 truncate font-mono" title={props.origin.name}>{props.origin.name}</code><span>·</span>
        <span class="min-w-0 truncate" title={triggerText(props.origin.trigger)}>{triggerText(props.origin.trigger)}</span><span>·</span>
        <span class={props.origin.outcome === "failed" ? "text-destructive" : "text-muted-foreground"}>{props.origin.outcome}</span>
        <time class="ml-auto shrink-0" datetime={props.message.ts}>{new Date(props.message.ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</time>
      </div>
      <div class="mt-1 text-base text-foreground">
        <Show when={json()} fallback={<p class="whitespace-pre-wrap break-words">{props.origin.error ?? props.message.body}</p>}><Markdown>{props.message.body}</Markdown></Show>
      </div>
    </ConversationNote>
  </article>;
}
