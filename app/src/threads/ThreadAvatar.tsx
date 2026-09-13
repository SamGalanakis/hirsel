import { createEffect, createSignal, Show } from "solid-js";
import { getClient } from "../ws/client";
import type { ThreadIcon } from "./types";

/** A durable Thread identity cue, independent of activity or lifecycle state. */
const tones = [
  "bg-emerald-100 text-emerald-900 dark:bg-emerald-950 dark:text-emerald-200",
  "bg-amber-100 text-amber-900 dark:bg-amber-950 dark:text-amber-200",
  "bg-sky-100 text-sky-900 dark:bg-sky-950 dark:text-sky-200",
  "bg-rose-100 text-rose-900 dark:bg-rose-950 dark:text-rose-200",
  "bg-violet-100 text-violet-900 dark:bg-violet-950 dark:text-violet-200",
];
export interface ThreadAvatarIdentity { id: number; kind: "space" | "task"; title: string; icon?: ThreadIcon | null }
/** `inline` is the prose variant: an em-relative box that rides the sentence it
 * cites instead of a fixed 16px chip standing beside it. Every other size is a
 * layout box in a list or a header, where a fixed pixel size is the point. */
export function ThreadAvatar(props: { thread: ThreadAvatarIdentity; small?: boolean; dense?: boolean; inline?: boolean; large?: boolean }) {
  const imageId = () => props.thread.icon?.kind === "image" ? props.thread.icon.blob_id : null;
  const [failed, setFailed] = createSignal(false);
  const [imageUrl, setImageUrl] = createSignal<string | null>(null);
  let request = 0;
  createEffect(imageId, blobId => {
    const current = ++request;
    setFailed(false);
    setImageUrl(null);
    const client = getClient();
    if (!blobId || !client) return;
    void client.getBlobUrl(blobId).then(url => {
      if (current === request && imageId() === blobId) setImageUrl(url);
    }).catch(() => {
      if (current === request) setFailed(true);
    });
  });
  const glyph = () => props.thread.icon?.kind === "emoji"
    ? props.thread.icon.value
    : (Array.from(props.thread.title.trim())[0]?.toLocaleUpperCase() || "#");
  return <span aria-hidden="true" data-slot="thread-avatar" data-thread-avatar={props.thread.id}
    data-thread-kind={props.thread.kind} class={`inline-flex shrink-0 select-none items-center justify-center overflow-hidden align-middle font-medium leading-none ${props.thread.kind === "space" ? "rounded-md" : "rounded-full"} ${props.inline ? "size-[1.15em] text-[0.62em]" : props.dense ? "size-4 text-meta" : props.small ? "size-5 text-xs" : props.large ? "size-16 text-3xl" : "size-7 text-sm"} ${tones[Math.abs(props.thread.id) % tones.length]}`}>
    <Show when={imageId() && imageUrl() && !failed()} fallback={glyph()}>
      <img src={imageUrl() ?? undefined} alt="" class="size-full object-cover" onError={() => setFailed(true)} />
    </Show>
  </span>;
}
