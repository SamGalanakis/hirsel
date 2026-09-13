import { createEffect, createSignal, For, Show } from "solid-js";
import { getClient } from "../ws/client";
import { THREAD_SYMBOL_ART } from "./thread-symbol-art";
import { threadMonogram, tintStyle, type ThreadSymbol, type ThreadTint } from "./thread-symbols";
import type { ThreadIcon } from "./types";

/** One vocabulary glyph, stroked at the tile's current colour. */
export function ThreadSymbolGlyph(props: { name: ThreadSymbol; class?: string }) {
  return <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor"
    stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"
    class={props.class ?? "size-4"}>
    <For each={THREAD_SYMBOL_ART[props.name]}>{d => <path d={d} />}</For>
  </svg>;
}

export interface ThreadAvatarIdentity { id: number; kind: "space" | "task"; title: string; icon?: ThreadIcon | null }
/** A durable Thread identity cue, independent of activity or lifecycle state:
 * a tinted vocabulary symbol, an uploaded square image, or — the default — a
 * quiet monogram of the title. Shape alone carries kind.
 *
 * `inline` is the prose variant: an em-relative box that rides the sentence it
 * cites instead of a fixed chip standing beside it. Every other size is a
 * layout box in a list or a header, where a fixed pixel size is the point.
 *
 * The monogram is type and obeys the ramp's floor: `text-meta` (11px) in the
 * 20px and 24px tiles, `text-xs` in the 28px header tile. The inline tile is
 * 1.3em of its sentence — 18px in prose, 16px in the `text-xs` trace — and the
 * letters stay `text-meta`; where the tile is too small to hold them (under
 * 17px, the trace) the tile itself hides its letters and stands as a plain
 * tinted mark, the same shape and tint, rather than shrinking the type below
 * the floor. */
const TILE = {
  // em-relative on purpose: the inline tile is sized by the sentence it rides.
  // The box keeps the sentence's font-size so its em is the sentence's em; the
  // letters alone step down to `text-meta`, and hide when the box is too small.
  inline: { box: "@container size-[1.3em]", glyph: "size-[0.8em]", letters: "text-meta @max-[17px]:hidden" },
  dense: { box: "size-5 text-meta", glyph: "size-3", letters: "" },
  small: { box: "size-6 text-meta", glyph: "size-3.5", letters: "" },
  header: { box: "size-7 text-xs", glyph: "size-4", letters: "" },
  large: { box: "size-16 text-2xl", glyph: "size-10", letters: "" },
} as const;
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
  const tile = () => TILE[props.inline ? "inline" : props.dense ? "dense" : props.small ? "small" : props.large ? "large" : "header"];
  return <span aria-hidden="true" data-slot="thread-avatar" data-thread-avatar={props.thread.id}
    data-thread-kind={props.thread.kind} data-thread-symbol={symbol()?.name} data-thread-tint={tint()}
    style={tintStyle(tint())}
    class={`inline-flex shrink-0 select-none items-center justify-center overflow-hidden align-middle font-semibold leading-none ${props.thread.kind === "space" ? "rounded-md" : "rounded-full"} ${tile().box}`}>
    <Show when={imageId() && imageUrl() && !failed()} fallback={
      <Show when={symbol()} fallback={<span class={tile().letters}>{threadMonogram(props.thread.title)}</span>}>
        {chosen => <ThreadSymbolGlyph name={chosen().name} class={tile().glyph} />}
      </Show>
    }>
      <img src={imageUrl() ?? undefined} alt="" class="size-full object-cover" onError={() => setFailed(true)} />
    </Show>
  </span>;
}
