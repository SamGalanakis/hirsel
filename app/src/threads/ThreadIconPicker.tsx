import { createEffect, createSignal, For, onCleanup, Show } from "solid-js";
import { createOverlayPresence } from "../lib/focus";
import { fileToBase64 } from "../lib/format";
import { historyId } from "../lib/history";
import { state } from "../store/store";
import { getClient, makeClientId } from "../ws/client";
import { ThreadAvatar } from "./ThreadAvatar";
import { threadIconError, threadIconPresets, threadIconTarget, setThreadIconTarget } from "./icon-picker";
import { threadAction } from "./store";
import { normalizeThreadIconImage, THREAD_ICON_MIMES } from "./thread-icon-image";
import type { ThreadIcon } from "./types";

const button = "inline-flex min-h-11 items-center justify-center rounded-lg px-3 text-sm hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-40";

export function ThreadIconPicker() {
  let dialog: HTMLDialogElement | undefined;
  let fileInput: HTMLInputElement | undefined;
  let restore: HTMLElement | null = null;
  const [emoji, setEmoji] = createSignal<string | null>(null);
  const [image, setImage] = createSignal<ThreadIcon & { kind: "image" } | null>(null);
  const [uploading, setUploading] = createSignal(false);
  const [uploadError, setUploadError] = createSignal<string | null>(null);
  const value = (): ThreadIcon | null => image() ?? (emoji() === null ? null : { kind: "emoji", value: emoji()! });
  const close = () => setThreadIconTarget(null);
  const error = () => image() ? null : threadIconError(emoji());
  const handleDragOver = (event: DragEvent) => { if (event.dataTransfer?.types.includes("Files")) event.preventDefault(); };
  const handleDrop = (event: DragEvent) => { const file = filesFromDrop(event); if (file) { event.preventDefault(); void upload(file); } };
  const handlePaste = (event: ClipboardEvent) => { const file = filesFromClipboard(event); if (file) { event.preventDefault(); void upload(file); } };
  createOverlayPresence(() => threadIconTarget() !== null);
  onCleanup(() => {
    dialog?.removeEventListener("dragover", handleDragOver);
    dialog?.removeEventListener("drop", handleDrop);
    dialog?.removeEventListener("paste", handlePaste);
    dialog?.close();
    close();
  });

  createEffect(() => ({ target: threadIconTarget(), history: historyId() }), ({ target, history }) => {
    if (target && target.history !== history) { close(); return; }
    if (target) {
      setEmoji(target.thread.icon?.kind === "emoji" ? target.thread.icon.value : null);
      setImage(target.thread.icon?.kind === "image" ? target.thread.icon : null);
      setUploadError(null);
      setUploading(false);
      restore = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      if (!dialog?.open) dialog?.showModal();
      const frame = requestAnimationFrame(() => dialog?.querySelector<HTMLInputElement>('input[type="text"]')?.focus());
      return () => cancelAnimationFrame(frame);
    } else if (dialog?.open) {
      dialog.close();
      if (restore?.isConnected) restore.focus();
    }
  });

  const upload = async (file: File | undefined) => {
    if (!file || uploading()) return;
    const target = threadIconTarget();
    const client = getClient();
    if (!target || !client || target.history !== historyId()) return;
    setUploading(true);
    setUploadError(null);
    try {
      const normalized = await normalizeThreadIconImage(file);
      const blob = await client.uploadBlob(makeClientId(), normalized.name, normalized.type, await fileToBase64(normalized));
      const current = threadIconTarget();
      if (current?.history === target.history && current.thread.id === target.thread.id && current.thread.revision === target.thread.revision) {
        setImage({ kind: "image", blob_id: blob.id });
      }
    } catch (cause) {
      setUploadError(cause instanceof Error ? cause.message : "Couldn’t prepare that image.");
    } finally {
      setUploading(false);
      if (fileInput) fileInput.value = "";
    }
  };
  const filesFromClipboard = (event: ClipboardEvent) => Array.from(event.clipboardData?.files ?? []).find(file => file.type.startsWith("image/"));
  const filesFromDrop = (event: DragEvent) => Array.from(event.dataTransfer?.files ?? []).find(file => file.type.startsWith("image/"));
  const chooseEmoji = (next: string | null) => { setImage(null); setEmoji(next); setUploadError(null); };
  const save = (event: SubmitEvent) => {
    event.preventDefault();
    const target = threadIconTarget();
    if (!target || target.history !== historyId() || error() || uploading() || state.connection !== "connected") return;
    threadAction(target.history, target.thread.id, "set_icon", { icon: value() }, target.thread.revision);
    close();
  };

  return <dialog ref={node => {
    dialog = node;
    node.addEventListener("dragover", handleDragOver);
    node.addEventListener("drop", handleDrop);
    node.addEventListener("paste", handlePaste);
  }} aria-label="Change thread icon"
    onCancel={event => { event.preventDefault(); close(); }}
    onKeyDown={event => { if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); close(); } }}
    class="m-auto max-h-[calc(100dvh-2rem)] w-[min(24rem,calc(100vw-2rem))] overflow-y-auto rounded-xl border border-border bg-background p-5 text-foreground backdrop:bg-black/30">
    <div role="group" aria-label="Thread icon choices">
    <Show when={threadIconTarget()}>{target => <form onSubmit={save} class="space-y-4">
      <h2 class="text-base font-semibold">Change thread icon</h2>
      <div class="flex items-center gap-2"><ThreadAvatar thread={{ ...target().thread, icon: value() }} /><span class="min-w-0 break-words text-sm">{target().thread.title}</span></div>
      <div class="grid grid-cols-6 gap-1" role="group" aria-label="Suggested icons"><For each={threadIconPresets}>{preset => <button type="button" class={`${button} px-0 text-xl aria-pressed:bg-muted`} aria-label={preset.label} aria-pressed={emoji() === preset.icon && !image() ? "true" : "false"} title={preset.label} onClick={() => chooseEmoji(preset.icon)}>{preset.icon}</button>}</For></div>
      <label class="block space-y-2 text-sm">Custom emoji or symbol<input type="text" value={emoji() ?? ""} onInput={event => chooseEmoji(event.currentTarget.value)} aria-invalid={error() ? "true" : undefined} aria-describedby={error() ? "thread-icon-error" : undefined} class="block min-h-11 w-full rounded-lg border border-border bg-transparent px-3 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" /></label>
      <div class="rounded-lg border border-dashed border-border p-3 text-sm">
        <div class="flex flex-wrap items-center justify-between gap-2">
          <span class="text-muted-foreground">PNG, JPEG, or WebP · cropped to 256 px</span>
          <button type="button" class={button} disabled={uploading()} onClick={() => fileInput?.click()}>{uploading() ? "Preparing…" : "Upload image"}</button>
        </div>
        <p class="mt-1 text-xs text-muted-foreground">You can also drop or paste an image here.</p>
        <input ref={node => { fileInput = node; }} type="file" accept={THREAD_ICON_MIMES.join(",")} class="sr-only" aria-label="Choose icon image" onChange={event => void upload(event.currentTarget.files?.[0])} />
        <Show when={image()}><button type="button" class={`${button} mt-2 text-status-danger`} onClick={() => setImage(null)}>Remove image</button></Show>
      </div>
      <Show when={error()}><p id="thread-icon-error" role="alert" class="text-sm text-status-danger">{error()}</p></Show>
      <Show when={uploadError()}><p role="alert" class="text-sm text-status-danger">{uploadError()}</p></Show>
      <div class="flex flex-wrap justify-between gap-2"><button type="button" class={button} onClick={() => chooseEmoji(null)}>Use default</button><div class="flex gap-1"><button type="button" class={button} onClick={close}>Cancel</button><button type="submit" class={`${button} bg-primary text-primary-foreground hover:bg-primary/90`} disabled={Boolean(error()) || uploading() || state.connection !== "connected"}>Save icon</button></div></div>
    </form>}</Show>
    </div>
  </dialog>;
}
