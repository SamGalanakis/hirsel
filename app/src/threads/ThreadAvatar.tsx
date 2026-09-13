import { createEffect, createSignal, For, Show } from "solid-js";
import { getClient } from "../ws/client";
import { THREAD_SYMBOL_ART } from "./thread-symbol-art";
import { threadMonogram, tintStyle, type ThreadSymbol, type ThreadTint } from "./thread-symbols";
import type { ThreadIcon } from "./types";

/** One vocabulary glyph, stroked at the tile's current colour. */
export function ThreadSymbolGlyph(props: { name: ThreadSymbol; class?: string }) {
  return <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor"
    stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"
    class={`size-[62%] ${props.class ?? ""}`}>
    <For each={THREAD_SYMBOL_ART[props.name]}>{d => <path d={d} />}</For>
  </svg>;
}

export interface ThreadAvatarIdentity { id: number; kind: "space" | "task"; title: string; icon?: ThreadIcon | null }
/** A durable Thread identity cue, independent of activity or lifecycle state:
 * a tinted vocabulary symbol, an uploaded square image, or — the default — a
 * quiet monogram of the title. Shape alone carries kind.
 *
 * `inline` is the prose variant: an em-relative box that rides the sentence it
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
  const symbol = () => props.thread.icon?.kind === "symbol" ? props.thread.icon : null;
  const tint = (): ThreadTint => symbol()?.tint ?? "neutral";
  return <span aria-hidden="true" data-slot="thread-avatar" data-thread-avatar={props.thread.id}
    data-thread-kind={props.thread.kind} data-thread-symbol={symbol()?.name} data-thread-tint={tint()}
    style={tintStyle(tint())}
    class={`inline-flex shrink-0 select-none items-center justify-center overflow-hidden align-middle font-semibold leading-none ${props.thread.kind === "space" ? "rounded-md" : "rounded-full"} ${props.inline ? "size-[1.15em] text-[0.6em]" : props.dense ? "size-4 text-[0.6rem]" : props.small ? "size-5 text-[0.66rem]" : props.large ? "size-16 text-2xl" : "size-7 text-xs"}`}>
    <Show when={imageId() && imageUrl() && !failed()} fallback={
      <Show when={symbol()} fallback={threadMonogram(props.thread.title)}>
        {chosen => <ThreadSymbolGlyph name={chosen().name} />}
      </Show>
    }>
      <img src={imageUrl() ?? undefined} alt="" class="size-full object-cover" onError={() => setFailed(true)} />
    </Show>
  </span>;
}
