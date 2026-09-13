import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { ArrowRight, Check, GitBranch, Layers, Paperclip, X } from "../components/ui/icons";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "../components/ui/dropdown-menu";
import { createMediaFlag, createOverlayPresence } from "../lib/focus";
import { historyId } from "../lib/history";
import { handleSubmitKeys } from "../lib/submitKeymap";
import { createComposerAttachments } from "../components/chat/useAttachments";
import { state } from "../store/store";
import { createThread, sendThreadMessage, threadState } from "./store";
import { closeThreadCreate, threadCreateIntent } from "./create";
import { threadPath } from "./tree";
import type { ThreadKind } from "./types";

const chip = "inline-flex h-7 min-w-0 items-center gap-1.5 rounded-full border border-border px-2.5 text-meta text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-40 pointer-coarse:h-11 pointer-coarse:text-sm";
/** ONE labelled-field shape for this dialog: the label above the box at meta
 * size, the box itself at the reading size with a real border and a real focus
 * ring. */
const fieldLabel = "flex flex-col gap-1 text-meta font-medium uppercase tracking-wide text-muted-foreground";
const fieldBox = "w-full min-w-0 rounded-lg border border-border bg-background px-2.5 text-sm font-normal normal-case tracking-normal text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring";
const icon = "inline-flex size-8 shrink-0 items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-40 pointer-coarse:size-11";
/** The Thread's name is the first thing the Owner wrote, unless they say otherwise. */
export function derivedTitle(body: string): string {
  const line = body.split("\n").map(part => part.trim()).find(part => part.length > 0) ?? "";
  return line.length > 72 ? `${line.slice(0, 71).trimEnd()}…` : line;
}
export function ThreadCreate(props: { onSelect: (id: number) => void }) {
  let dialog: HTMLDialogElement | undefined;
  let textarea: HTMLTextAreaElement | undefined;
  let fileInput: HTMLInputElement | undefined;
  let restoreTarget: HTMLElement | null = null;
  const attachments = createComposerAttachments();
  const coarse = createMediaFlag("(pointer: coarse)");
  const [body, setBody] = createSignal("");
  const [name, setName] = createSignal("");
  const [kind, setKind] = createSignal<ThreadKind>("space");
  const [parentId, setParentId] = createSignal<number | null>(null);
  const [creating, setCreating] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const open = () => threadCreateIntent() !== null;
  const parent = createMemo(() => threadState.threads.find(thread => thread.id === parentId()));
  // A Task holds finishable work only, so a Task parent fixes the child's kind.
  const canBeSpace = () => parentId() === null || parent()?.kind === "space";
  const resolvedKind = () => canBeSpace() ? kind() : "task";
  const title = () => name().trim() || derivedTitle(body());
  createOverlayPresence(open);
  onCleanup(() => dialog?.close());
  createEffect(() => threadCreateIntent(), intent => {
    if (!dialog) return;
    if (!intent) {
      if (dialog.open) dialog.close();
      const destination = restoreTarget?.isConnected ? restoreTarget : null;
      restoreTarget = null;
      destination?.focus();
      return;
    }
    setParentId(intent.parentId);
    setError(null);
    if (!dialog.open) {
      restoreTarget = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      dialog.showModal();
    }
    const frame = requestAnimationFrame(() => textarea?.focus());
    return () => cancelAnimationFrame(frame);
  });
  const dismiss = (event?: Event) => { event?.preventDefault(); closeThreadCreate(); };
  const submit = async () => {
    const history = historyId();
    if (creating() || !title()) return;
    if (!history) { setError("History is unavailable. Reconnect and try again."); return; }
    setCreating(true); setError(null);
    try {
      // No attachments means no await before the create frame, so the Thread is
      // requested in the same tick the Owner pressed the button.
      const blobs = attachments.files().length > 0 ? await attachments.uploadAll() : [];
      const thread = await createThread(history, title(), resolvedKind(), parentId());
      // Creation carries a name only; the brief travels as the Thread's first
      // message, which is the same contract the composer uses.
      if (body().trim() || blobs.length > 0) sendThreadMessage(history, thread.id, body().trim(), "send", blobs, [], []);
      attachments.clear(); setBody(""); setName(""); setKind("space");
      closeThreadCreate();
      props.onSelect(thread.id);
    } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)); }
    finally { setCreating(false); }
  };
  /** The same condition the create control carries, so Enter and the button
   * agree about when the Thread can be made. */
  const ready = () => Boolean(title()) && !creating() && state.connection === "connected";
  const parents = createMemo(() => threadState.threads.filter(thread => !thread.archived_at).slice(0, 50));
  return <dialog ref={node => { dialog = node; }} aria-label="New Space or Task" data-slot="thread-create"
    onCancel={dismiss} onKeyDown={event => { if (event.key === "Escape") { event.stopPropagation(); dismiss(event); } }}
    onPointerDown={event => { if (event.target === dialog) dismiss(event); }}
    class="m-auto w-[min(38rem,calc(100vw-2rem))] max-w-none flex-col rounded-xl border border-border bg-surface p-0 text-foreground shadow-raised backdrop:bg-scrim open:flex max-sm:h-dvh max-sm:max-h-none max-sm:w-screen max-sm:rounded-none">
    {/* Closed, the modal holds no fields: the draft lives in this component's
        state, so nothing outside it can find a stray textarea. */}
    <Show when={open()}><div class="flex min-h-0 flex-1 flex-col gap-2 p-3">
      {/* The dialog says what it is before it asks for anything, and both
          fields carry a visible label at ONE size with a visible focus ring.
          They used to be two bare placeholder-only boxes with no title above
          them and no ring, so an empty dialog read as a blank card and a
          keyboard user could not see where they were. */}
      <div class="flex items-start gap-2">
        <h2 class="min-w-0 flex-1 text-lg font-medium text-foreground">New Space or Task</h2>
        <button type="button" class="inline-flex size-7 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:size-11" aria-label="Close" title="Close" onClick={dismiss}><X class="size-4" /></button>
      </div>
      {/* A single-line name: Enter ends it, on every pointer. The first-message
          textarea keeps its own rule, where Enter is a newline on touch. */}
      <label class={fieldLabel}>Name
        <input aria-label="New space or task title" placeholder={derivedTitle(body()) || "Optional — the first message names it"} value={name()} onInput={event => setName(event.currentTarget.value)}
          onKeyDown={event => { if (event.key !== "Enter" || event.shiftKey || event.isComposing) return; event.preventDefault(); if (ready()) void submit(); }}
          class={`${fieldBox} h-9 pointer-coarse:h-11`} />
      </label>
      <label class={`${fieldLabel} min-h-0 flex-1`}>First message
        <textarea ref={node => { textarea = node; }} aria-label="First message" placeholder="What is this about? Your first message starts it off…" value={body()} onInput={event => setBody(event.currentTarget.value)}
          onKeyDown={event => handleSubmitKeys(event, { value: body, coarse, onSend: () => void submit() })}
          class={`${fieldBox} min-h-24 flex-1 resize-none py-2`} />
      </label>
      <Show when={attachments.files().length > 0}>
        <ul data-slot="create-attachments" class="flex flex-wrap gap-1.5">
          <For each={attachments.files()}>{file => <li class="inline-flex items-center gap-1 rounded-full border border-border px-2 py-0.5 text-meta text-muted-foreground">
            <span class="max-w-40 truncate">{file.name}</span>
            <button type="button" class="rounded-full p-0.5 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" aria-label={`Remove ${file.name}`} onClick={() => attachments.removeFile(file.clientId)}><X class="size-3" /></button>
          </li>}</For>
        </ul>
      </Show>
      <Show when={error()}><p role="alert" class="px-1 text-meta text-destructive">{error()}</p></Show>
    </div>
    {/* The chips shrink and truncate before anything wraps, and the primary
        button is `ml-auto` so that when a phone width does wrap it, it lands
        whole on its own right-hugging line instead of clipping. */}
    <footer class="flex flex-wrap items-center gap-1.5 border-t border-border/60 px-3 py-2">
      <Show when={canBeSpace()}>
        <DropdownMenu>
          <DropdownMenuTrigger class={`${chip} shrink-0`} aria-label={`Kind: ${resolvedKind() === "space" ? "Space" : "Task"}`} title="Spaces hold ongoing context. Tasks hold work you can mark done."><Layers class="size-3.5" />{resolvedKind() === "space" ? "Space" : "Task"}</DropdownMenuTrigger>
          <DropdownMenuContent>
            <For each={["space", "task"] as const}>{option => <DropdownMenuItem role="menuitemradio" aria-checked={resolvedKind() === option ? "true" : "false"} class="min-h-11 capitalize" onSelect={() => setKind(option)}><Check class={resolvedKind() === option ? "size-4" : "size-4 invisible"} />{option}</DropdownMenuItem>}</For>
          </DropdownMenuContent>
        </DropdownMenu>
      </Show>
      <DropdownMenu>
        <DropdownMenuTrigger class={chip} aria-label={`Inside: ${parent() ? parent()!.title : "Top level"}`} title={parent() ? threadPath(threadState.threads, parent()!.id) : "Top level"}><GitBranch class="size-3.5" /><span class="truncate">{parent()?.title ?? "Top level"}</span></DropdownMenuTrigger>
        <DropdownMenuContent>
          <DropdownMenuItem class="min-h-11" onSelect={() => setParentId(null)}><Check class={parentId() === null ? "size-4" : "size-4 invisible"} />Top level</DropdownMenuItem>
          <For each={parents()}>{thread => <DropdownMenuItem class="min-h-11" onSelect={() => setParentId(thread.id)}><Check class={parentId() === thread.id ? "size-4" : "size-4 invisible"} /><span class="truncate">{thread.title}</span></DropdownMenuItem>}</For>
        </DropdownMenuContent>
      </DropdownMenu>
      <input ref={node => { fileInput = node; }} type="file" multiple class="hidden" onChange={event => { if (event.currentTarget.files) attachments.addFiles(event.currentTarget.files); event.currentTarget.value = ""; }} />
      <button type="button" class={icon} aria-label="Attach files" title="Attach files" onClick={() => fileInput?.click()}><Paperclip class="size-4" /></button>
      <button type="button" data-slot="create-submit" class="ml-auto inline-flex min-h-9 shrink-0 items-center gap-1.5 rounded-lg bg-primary px-3 text-sm font-medium text-primary-foreground transition-colors hover:bg-primary/90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-50 pointer-coarse:min-h-11" title={resolvedKind() === "space" ? "Create the Space" : "Create the Task"}
        disabled={!ready()} onClick={() => void submit()}>{resolvedKind() === "space" ? "Create Space" : "Create Task"}<ArrowRight class="size-4" /></button>
    </footer></Show>
  </dialog>;
}
