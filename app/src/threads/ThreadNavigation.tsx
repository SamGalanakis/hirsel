import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { createOverlayPresence } from "../lib/focus";
import { Plus, X } from "../components/ui/icons";
import { threadSection, type ThreadSection } from "./model";
import { ThreadStatus } from "./ThreadStatus";
import { ThreadError } from "./ThreadError";
import { ThreadActions } from "./ThreadActions";
import { createThread, threadState } from "./store";

const control = "inline-flex size-11 shrink-0 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-40";
export function ThreadNavigation(props: { open: boolean; onClose: () => void; onSelect: (id: number) => void }) {
  let dialog: HTMLDialogElement | undefined;
  let restoreTarget: HTMLElement | null = null;
  const [section, setSection] = createSignal<ThreadSection>("active");
  const [title, setTitle] = createSignal("");
  const [creating, setCreating] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [now, setNow] = createSignal(Date.now());
  onCleanup(() => dialog?.close());
  createEffect(() => props.open, open => {
    if (!open) return;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(timer);
  });
  createOverlayPresence(() => props.open);
  createEffect(() => props.open, open => {
    if (!dialog) return;
    if (open) {
      restoreTarget = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      const focused = threadState.threads.find(thread => thread.id === threadState.focusedId);
      if (focused) setSection(threadSection(focused, now()));
      dialog.showModal();
      const frame = requestAnimationFrame(() => {
        if (!dialog?.open) return;
        const selected = dialog.querySelector<HTMLButtonElement>(`[data-thread-row="${threadState.focusedId}"]`);
        (selected ?? dialog.querySelector<HTMLInputElement>("input"))?.focus();
      });
      return () => cancelAnimationFrame(frame);
    } else if (dialog.open) {
      dialog.close();
      const destination = restoreTarget?.isConnected ? restoreTarget : document.querySelector<HTMLElement>('[data-slot="thread-navigation-trigger"]');
      destination?.focus();
    }
  });
  const threads = createMemo(() => threadState.threads.filter(thread => thread.id !== 0 && threadSection(thread, now()) === section()));
  let focusedRowIndex = 0;
  createEffect(() => threads().map(thread => thread.id).join(","), () => {
    const frame = requestAnimationFrame(() => {
      if (!dialog?.open || document.activeElement !== document.body) return;
      const rows = dialog.querySelectorAll<HTMLButtonElement>("[data-thread-row]");
      (rows[Math.min(focusedRowIndex, rows.length - 1)] ?? dialog.querySelector<HTMLButtonElement>('[aria-label="Thread visibility"] button[aria-pressed="true"]'))?.focus();
    });
    return () => cancelAnimationFrame(frame);
  });
  const move = (event: KeyboardEvent) => {
    const target = event.target as HTMLElement;
    if (!target.matches("[data-thread-row]")) return;
    const rows = Array.from(dialog?.querySelectorAll<HTMLButtonElement>("[data-thread-row]") ?? []);
    const current = rows.indexOf(target as HTMLButtonElement);
    const next = event.key === "ArrowDown" ? Math.min(current + 1, rows.length - 1) : event.key === "ArrowUp" ? Math.max(0, current - 1) : event.key === "Home" ? 0 : event.key === "End" ? rows.length - 1 : null;
    if (next !== null) { event.preventDefault(); rows[next]?.focus(); }
  };
  const add = async (event: SubmitEvent) => {
    event.preventDefault();
    const value = title().trim();
    if (!value || creating()) return;
    setCreating(true); setError(null);
    try { const thread = await createThread(value); setTitle(""); setSection("active"); props.onSelect(thread.id); }
    catch (cause) { setError(String(cause)); }
    finally { setCreating(false); }
  };
  const dismiss = (event: Event) => { event.preventDefault(); props.onClose(); };
  return <dialog ref={node => { dialog = node; }} id="thread-navigation" aria-label="Threads" data-slot="thread-drawer"
    onCancel={dismiss} onKeyDown={event => { if (event.key === "Escape") { event.stopPropagation(); dismiss(event); } }}
    onPointerDown={event => { if (event.target === dialog && dialog) { const rect = dialog.getBoundingClientRect(); if (event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom) props.onClose(); } }}
    class="fixed inset-y-0 left-14 m-0 h-dvh max-h-none w-[min(22rem,calc(100vw-3.5rem))] max-w-none flex-col border-0 border-r border-border bg-background p-4 text-foreground backdrop:bg-transparent open:flex">
    <div class="mb-4 flex items-center justify-between"><h2 class="text-base font-semibold">Threads</h2><button class={control} aria-label="Close threads" title="Close threads" onClick={props.onClose}><X class="size-5" /></button></div>
    <form onSubmit={event => void add(event)} class="flex gap-2"><input aria-label="New thread title" placeholder="New thread…" value={title()} onInput={event => setTitle(event.currentTarget.value)} class="min-w-0 flex-1 rounded-lg border border-border bg-transparent px-3 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" /><button type="submit" class={`${control} bg-muted text-foreground`} aria-label="Create thread" title="Create thread" disabled={!title().trim() || creating()}><Plus class="size-5" /></button></form>
    <Show when={error()}><div role="alert" class="mt-3 space-y-2 text-sm"><p>Couldn’t confirm the new thread. Your title is kept. Check the list before creating it again.</p><details><summary class="cursor-pointer py-2 text-muted-foreground">Technical details</summary><p class="break-words">{error()}</p></details></div></Show>
    <div class="my-3 grid grid-cols-4 gap-1" aria-label="Thread visibility"><For each={["active", "settled", "snoozed", "archived"] as const}>{name => <button class="min-h-11 min-w-0 rounded-lg px-1 text-xs capitalize text-muted-foreground hover:bg-muted aria-pressed:bg-muted aria-pressed:font-medium aria-pressed:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" aria-pressed={section() === name ? "true" : "false"} onClick={() => setSection(name)}>{name}</button>}</For></div>
    <ThreadError />
    <nav aria-label="Thread inventory" class="min-h-0 flex-1 overflow-y-auto" onFocusIn={event => { const row = (event.target as HTMLElement).closest('[data-thread-entry]'); if (row) focusedRowIndex = threads().findIndex(thread => thread.id === Number(row.getAttribute('data-thread-entry'))); }}><ul class="flex flex-col gap-1">
      <For each={threads()}>{thread => <li data-thread-entry={thread.id} class="flex shrink-0 items-start rounded-lg hover:bg-muted/50"><button onContextMenu={event => { event.preventDefault(); event.currentTarget.parentElement?.querySelector<HTMLButtonElement>('[data-thread-actions]')?.click(); }} onKeyDown={event => { move(event); if (event.key === "ContextMenu" || (event.key === "F10" && event.shiftKey)) { event.preventDefault(); event.currentTarget.parentElement?.querySelector<HTMLButtonElement>('[data-thread-actions]')?.click(); } }} data-thread-row={thread.id} class="block min-h-11 min-w-0 flex-1 rounded-lg px-3 py-3 text-left text-sm text-muted-foreground hover:bg-muted aria-current:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" aria-current={threadState.focusedId === thread.id ? "page" : undefined} onClick={() => props.onSelect(thread.id)}>
        <span class="block break-words font-medium text-foreground">{thread.title}</span>
        <span class="mt-1 flex items-start gap-2"><span class="text-xs tabular-nums">#{thread.id}</span><Show when={!thread.read}><span role="img" aria-label="Unread" title="Unread" class="mt-1 size-1.5 shrink-0 rounded-full bg-primary" /></Show><ThreadStatus thread={thread} now={now()} /></span>
      </button><ThreadActions thread={thread} quick now={now()} /></li>}</For></ul>
      <Show when={threads().length === 0}><p class="p-3 text-sm text-muted-foreground">No {section()} threads</p></Show>
    </nav>
  </dialog>;
}
