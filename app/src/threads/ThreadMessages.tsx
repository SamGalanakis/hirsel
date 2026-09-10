import { For, onCleanup, Show } from "solid-js";
import { Markdown } from "../components/Markdown";
import { BrandMark } from "../components/BrandMark";
import { UserRound } from "../components/ui/icons";
import { ArtifactCard } from "../artifacts/ArtifactSurface";
import { getClient } from "../ws/client";
import { splitStreamingReply } from "../components/chat/timeline";
import { ActivityEntry, ThreadWork } from "./ThreadWork";
import type { ConversationEntry } from "./conversation";
import type { ThreadHistory } from "./model";
import { threadState } from "./store";
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
  const events = () => turn() === undefined ? [] : threadState.turnDetails[turn()!.id] ?? (threadState.streamTurnIds[props.threadId] === turn()!.id ? threadState.streams[props.threadId] ?? [] : []);
  return <>
    <Show when={props.entry.kind !== "activity"}>
      <article ref={node => { releaseFocus = preserveMovedFocus(node); }} data-message-id={message()?.id} data-execution-turn={!message() ? turn()?.id : undefined} aria-label={owner() ? "You" : "Hirsel"} class={["flex items-start gap-3", owner() ? "flex-row-reverse" : ""]}>
        <span class="grid size-8 shrink-0 place-items-center rounded-full bg-muted/45" aria-hidden="true"><Show when={owner()} fallback={<BrandMark size={22} />}><UserRound class="size-4 text-muted-foreground" /></Show></span>
        <div class={owner() ? "min-w-0 max-w-[85%] rounded-xl bg-muted/65 px-4 py-3" : "min-w-0 flex-1 pt-1"}>
          <Show when={!owner()}><ThreadWork message={message()} turn={turn()} activities={activities(turn()?.id)} events={events()} live={turn()?.state === "running"} /></Show>
          <Markdown>{message()?.body ?? splitStreamingReply(events()).reply}</Markdown>
          <For each={message()?.artifact_ids ?? []}>{id => <ArtifactCard id={id} />}</For>
          <Show when={message()?.attachments?.length}><ul class="mt-2 text-xs text-muted-foreground"><For each={message()?.attachments}>{blob => <li><button class="underline" onClick={() => { void getClient()?.getBlobUrl(blob.id).then(url => window.open(url, "_blank", "noopener,noreferrer")); }}>{blob.name}</button></li>}</For></ul></Show>
        </div>
      </article>
    </Show>
    <Show when={props.entry.kind === "activity"}><ActivityEntry activity={activity()} /></Show>
  </>;
}
