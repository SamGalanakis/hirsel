import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { createOverlayPresence } from "../lib/focus";
import { fileToBase64 } from "../lib/format";
import { historyId } from "../lib/history";
import { state } from "../store/store";
import { getClient, makeClientId } from "../ws/client";
import { ThreadAvatar, ThreadSymbolGlyph } from "./ThreadAvatar";
import { threadIconTarget, setThreadIconTarget } from "./icon-picker";
import { threadAction } from "./store";
import { normalizeThreadIconImage, THREAD_ICON_MIMES } from "./thread-icon-image";
import { matchesThreadSymbol, THREAD_SYMBOL_GROUPS, THREAD_TINTS, tintStyle, type ThreadSymbol, type ThreadTint } from "./thread-symbols";
import type { ThreadIcon } from "./types";

const quiet = "inline-flex h-8 items-center justify-center rounded-md px-2 text-meta text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-40 pointer-coarse:h-11 pointer-coarse:text-sm";
const action = "inline-flex h-8 items-center justify-center rounded-md px-3 text-sm transition-colors hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-40 pointer-coarse:h-11";
const swatch = "inline-flex size-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring aria-pressed:bg-primary/15 aria-pressed:text-foreground aria-pressed:ring-1 aria-pressed:ring-primary/50 pointer-coarse:size-11";

/** One dialog: a live preview that is also the dropzone, then the choices. */
export function ThreadIconPicker() {
  let dialog: HTMLDialogElement | undefined;
  let fileInput: HTMLInputElement | undefined;
  let restore: HTMLElement | null = null;
  const [symbol, setSymbol] = createSignal<ThreadSymbol | null>(null);
  const [tint, setTint] = createSignal<ThreadTint>("neutral");
  const [image, setImage] = createSignal<ThreadIcon & { kind: "image" } | null>(null);
  const [query, setQuery] = createSignal("");
  const [uploading, setUploading] = createSignal(false);
  const [uploadError, setUploadError] = createSignal<string | null>(null);
  const [dragging, setDragging] = createSignal(false);
  const value = (): ThreadIcon | null => image() ?? (symbol() === null ? null : { kind: "symbol", name: symbol()!, tint: tint() });
  const close = () => setThreadIconTarget(null);
  const groups = createMemo(() => THREAD_SYMBOL_GROUPS
    .map(group => ({ label: group.label, names: group.names.filter(name => matchesThreadSymbol(name, query())) }))
    .filter(group => group.names.length > 0));
  const empty = () => groups().length === 0;
  // "Nothing changed" is the saved icon, compared by its wire shape.
  const same = (left: ThreadIcon | null, right: ThreadIcon | null) => left === right
    || (left?.kind === "symbol" && right?.kind === "symbol" && left.name === right.name && left.tint === right.tint)
    || (left?.kind === "image" && right?.kind === "image" && left.blob_id === right.blob_id);
  const changed = () => !same(value(), threadIconTarget()?.thread.icon ?? null);
  const caption = () => uploading() ? "Preparing…" : image() ? "Uploaded image" : symbol() ?? "Monogram default";
  const handleDragOver = (event: DragEvent) => { if (event.dataTransfer?.types.includes("Files")) { event.preventDefault(); setDragging(true); } };
  const handleDragLeave = (event: DragEvent) => { if (!(event.relatedTarget instanceof Node) || !dialog?.contains(event.relatedTarget)) setDragging(false); };
  const handleDrop = (event: DragEvent) => { setDragging(false); const file = filesFromDrop(event); if (file) { event.preventDefault(); void upload(file); } };
  const handlePaste = (event: ClipboardEvent) => { const file = filesFromClipboard(event); if (file) { event.preventDefault(); void upload(file); } };
  createOverlayPresence(() => threadIconTarget() !== null);
  onCleanup(() => {
    dialog?.removeEventListener("dragover", handleDragOver);
    dialog?.removeEventListener("dragleave", handleDragLeave);
    dialog?.removeEventListener("drop", handleDrop);
    dialog?.removeEventListener("paste", handlePaste);
    dialog?.close();
    close();
  });

  createEffect(() => ({ target: threadIconTarget(), history: historyId() }), ({ target, history }) => {
    if (target && target.history !== history) { close(); return; }
    if (target) {
      const saved = target.thread.icon;
      setSymbol(saved?.kind === "symbol" ? saved.name : null);
      setTint(saved?.kind === "symbol" ? saved.tint : "neutral");
      setImage(saved?.kind === "image" ? saved : null);
      setQuery("");
      setUploadError(null);
      setUploading(false);
      setDragging(false);
      restore = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      if (!dialog?.open) dialog?.showModal();
      const frame = requestAnimationFrame(() => dialog?.querySelector<HTMLInputElement>('input[type="search"]')?.focus());
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
        setSymbol(null);
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
  const chooseSymbol = (next: ThreadSymbol | null) => { setImage(null); setSymbol(next); setUploadError(null); };
  const chooseTint = (next: ThreadTint) => { setImage(null); setTint(next); setUploadError(null); };
  const save = (event: SubmitEvent) => {
    event.preventDefault();
    const target = threadIconTarget();
    if (!target || target.history !== historyId() || uploading() || !changed() || state.connection !== "connected") return;
    threadAction(target.history, target.thread.id, "set_icon", { icon: value() }, target.thread.revision);
    close();
  };

  return <dialog ref={node => {
    dialog = node;
    node.addEventListener("dragover", handleDragOver);
    node.addEventListener("dragleave", handleDragLeave);
    node.addEventListener("drop", handleDrop);
    node.addEventListener("paste", handlePaste);
  }} aria-label="Change thread icon" data-slot="thread-icon-picker"
    onCancel={event => { event.preventDefault(); close(); }}
    onKeyDown={event => { if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); close(); } }}
    onPointerDown={event => { if (event.target === dialog) close(); }}
    class="m-auto max-h-[calc(100dvh-2rem)] w-[min(25rem,calc(100vw-2rem))] max-w-none flex-col overflow-hidden rounded-xl border border-border bg-surface p-0 text-foreground shadow-raised backdrop:bg-background/70 open:flex">
    <Show when={threadIconTarget()}>{target => <form onSubmit={save} class="flex min-h-0 flex-col">
      <div class="flex flex-col gap-3 p-4 pb-2">
        <h2 class="text-sm font-semibold">Change thread icon</h2>
        <div class="flex items-center gap-3">
          <button type="button" data-slot="thread-icon-preview" data-dragging={dragging() ? "true" : undefined}
            aria-label="Upload icon image" title="Drop, paste, or click to upload"
            class={`inline-flex shrink-0 rounded-xl border border-dashed p-1.5 transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring ${dragging() ? "border-primary bg-primary/10" : "border-border/70 hover:border-border hover:bg-muted/60"}`}
            onClick={() => fileInput?.click()}>
            <ThreadAvatar thread={{ ...target().thread, icon: value() }} large />
          </button>
          <div class="flex min-w-0 flex-1 flex-col gap-0.5">
            <span class="truncate text-sm">{target().thread.title}</span>
            <span class="flex min-w-0 items-center gap-1 text-meta text-muted-foreground">
              <span class="truncate">{caption()}</span>
              <Show when={image()}><button type="button" aria-label="Remove image" class="rounded px-1 underline decoration-dotted underline-offset-2 transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" onClick={() => { setImage(null); setUploadError(null); }}>Remove</button></Show>
            </span>
            <span class="text-meta text-muted-foreground">Drop or click the tile to upload an image</span>
          </div>
        </div>
        <Show when={uploadError()}><p id="thread-icon-error" role="alert" class="text-meta text-status-danger">{uploadError()}</p></Show>
        <input ref={node => { fileInput = node; }} type="file" accept={THREAD_ICON_MIMES.join(",")} class="hidden" aria-label="Choose icon image" onChange={event => void upload(event.currentTarget.files?.[0])} />
        <div class="flex items-center gap-1.5">
          <input type="search" aria-label="Search symbols" placeholder="Search symbols" value={query()} onInput={event => setQuery(event.currentTarget.value)}
            class="h-8 min-w-0 flex-1 rounded-md border border-border bg-transparent px-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:h-11" />
        </div>
        <div class="flex flex-wrap items-center gap-1" role="group" aria-label="Tint">
          <For each={THREAD_TINTS}>{name => <button type="button" aria-label={name} title={name} aria-pressed={tint() === name ? "true" : "false"}
            data-thread-tint={name} style={tintStyle(name)}
            class="size-6 rounded-full border border-border/60 transition-transform focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring aria-pressed:ring-2 aria-pressed:ring-primary/70 pointer-coarse:size-9"
            onClick={() => chooseTint(name)} />}</For>
        </div>
      </div>
      <div class="min-h-0 flex-1 overflow-y-auto px-4 pb-3">
        <Show when={!empty()} fallback={<p class="py-6 text-center text-meta text-muted-foreground">No symbol matches “{query()}”.</p>}>
          <For each={groups()}>{group => <section class="pt-2">
            <h3 class="pb-1 text-meta uppercase tracking-wide text-muted-foreground">{group.label}</h3>
            <div class="flex flex-wrap gap-1" role="group" aria-label={group.label}>
              <For each={group.names}>{name => <button type="button" class={swatch} aria-label={name} title={name}
                aria-pressed={symbol() === name && !image() ? "true" : "false"} onClick={() => chooseSymbol(name)}>
                <ThreadSymbolGlyph name={name} class="size-4" />
              </button>}</For>
            </div>
          </section>}</For>
        </Show>
      </div>
      <footer class="flex items-center gap-1.5 border-t border-border/60 px-3 py-2">
        <button type="button" class={quiet} onClick={() => chooseSymbol(null)}>Use default</button>
        <div class="flex-1" />
        <button type="button" class={action} onClick={close}>Cancel</button>
        <button type="submit" aria-label="Save icon" class={`${action} bg-primary text-primary-foreground hover:bg-primary/90`}
          disabled={uploading() || !changed() || state.connection !== "connected"}>Save</button>
      </footer>
    </form>}</Show>
  </dialog>;
}
