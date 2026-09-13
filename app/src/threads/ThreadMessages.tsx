import { For, onCleanup, Show } from "solid-js";
import { Markdown } from "../components/Markdown";
import { BrandMark } from "../components/BrandMark";
import { LoaderCircle, UserRound } from "../components/ui/icons";
import { ArtifactCard } from "../artifacts/ArtifactSurface";
import { getClient } from "../ws/client";
import { buildTimeline, splitStreamingReply } from "../components/chat/timeline";
import { ActivityEntry, ThreadWork, WorkTail } from "./ThreadWork";
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
  const turn = () => props.entry.kind === "activity" ? undefined : props.entry.turn;
  const owner = () => message()?.author === "owner";
  const activity = () => (props.entry as Extract<ConversationEntry, {kind:"activity"}>).activity;
  const activities = (id: number | undefined) => id === undefined ? [] : props.history.activities.filter(activity => activity.turn_id === id);
  const events = () => turn() === undefined ? [] : threadState.turnDetails[turn()!.id] ?? [];
  const split = () => splitStreamingReply(events());
  /** A turn that has started but said nothing yet is not a card: an empty box
   * with a spinner in its corner claims the Agent produced something. Until the
   * first reasoning line, pill or word arrives it is the avatar alone, turning. */
  const pending = () => !owner() && !message() && turn()?.state === "running"
    && activities(turn()?.id).length === 0 && split().reply === "" && buildTimeline(split().activity).length === 0;
  return <>
    <Show when={props.entry.kind !== "activity"}>
      {/* Who is speaking is readable before a word is: the Owner sits right in
          the filled emphasis pair (a near-white fill on the dark theme, the
          accent on the light one) at conversational width, the Agent sits left
          on a neutral surface that hugs its own content, so the two never share an
          edge and the column reads left against right. Wide work rows and code
          scroll inside the card, never the page. */}
      <article ref={node => { releaseFocus = preserveMovedFocus(node); }} data-message-id={message()?.id} data-execution-turn={!message() ? turn()?.id : undefined} data-author={owner() ? "owner" : "agent"} aria-label={owner() ? "You" : "Hirsel"} class={["flex items-start gap-2 sm:gap-3", owner() ? "flex-row-reverse" : ""]}>
        <span data-slot="message-avatar" data-pending={pending() ? "true" : undefined} role={pending() ? "status" : undefined} aria-hidden={pending() ? undefined : "true"}
          class="relative grid size-8 shrink-0 place-items-center rounded-full bg-muted/45">
          <Show when={owner()} fallback={<BrandMark size={22} />}><UserRound class="size-4 text-muted-foreground" /></Show>
          <Show when={pending()}>
            <LoaderCircle class={`absolute inset-0 size-8 text-muted-foreground/60 ${state.connection === "connected" ? "animate-spin motion-reduce:animate-none" : ""}`} aria-hidden="true" />
            <span class="sr-only">{workLabel(turn(), events(), activities(turn()?.id), 0, false)}</span>
          </Show>
        </span>
        <Show when={!pending()}>
        <div data-slot={owner() ? "owner-message" : "agent-message"} class={owner() ? "min-w-0 max-w-[85%] rounded-xl rounded-br-sm bg-primary px-3.5 py-2.5 text-primary-foreground [&_code]:bg-current/10 sm:max-w-[60%]" : "min-w-0 max-w-[92%] rounded-xl rounded-bl-sm border border-border/60 bg-surface px-3.5 py-2.5 sm:max-w-[80%]"}>
          <Show when={!owner()}><ThreadWork message={message()} turn={turn()} activities={activities(turn()?.id)} events={events()} live={turn()?.state === "running"} /></Show>
          <Markdown>{message()?.body ?? split().reply}</Markdown>
          <For each={message()?.artifact_ids ?? []}>{id => <ArtifactCard id={id} />}</For>
          <Show when={message()?.attachments?.length}><ul class="mt-2 text-xs text-muted-foreground"><For each={message()?.attachments}>{blob => <li><button class="underline" onClick={() => { void getClient()?.getBlobUrl(blob.id).then(url => window.open(url, "_blank", "noopener,noreferrer")); }}>{blob.name}</button></li>}</For></ul></Show>
          <Show when={!owner()}><WorkTail message={message()} turn={turn()} activities={activities(turn()?.id)} events={events()} /></Show>
        </div>
        </Show>
      </article>
    </Show>
    <Show when={props.entry.kind === "activity"}><ActivityEntry activity={activity()} /></Show>
  </>;
}
