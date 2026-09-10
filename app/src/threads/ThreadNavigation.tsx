import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { createOverlayPresence } from "../lib/focus";
import { Plus, X, Search, Funnel, Check, ChevronRight, Pin } from "../components/ui/icons";
import { type ThreadSection } from "./model";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "../components/ui/dropdown-menu";
import { openThreadSearch } from "../lib/keymap";
import type { ThreadNavigationIntent } from "./navigation";
import { ThreadAvatar } from "./ThreadAvatar";
import { ThreadStatus } from "./ThreadStatus";
import { ThreadError } from "./ThreadError";
import { ThreadActions } from "./ThreadActions";
import { threadAncestors, threadPath, threadTree } from "./tree";
import { openThreadNavigation } from "./navigation";
import { state } from "../store/store";
import { createThread, threadState } from "./store";

const control = "inline-flex size-11 shrink-0 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-40";
export function ThreadNavigation(props: { intent: ThreadNavigationIntent | null; onClose: () => void; onSelect: (id: number) => void }) {
  let dialog: HTMLDialogElement | undefined;
  let restoreTarget: HTMLElement | null = null;
  const [section, setSection] = createSignal<ThreadSection>("active");
  const [expanded, setExpanded] = createSignal<ReadonlySet<number>>(new Set());
  const [parentId, setParentId] = createSignal<number | null>(null);
  const [title, setTitle] = createSignal("");
  const [creating, setCreating] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [now, setNow] = createSignal(Date.now());
  onCleanup(() => dialog?.close());
  createEffect(() => props.intent !== null, open => {
    if (!open) return;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(timer);
  });
  createOverlayPresence(() => props.intent !== null);
  createEffect(() => props.intent, intent => {
    if (!dialog) return;
    if (intent) {
      setParentId(intent.kind === "create" ? intent.parentId : null);
      setExpanded(previous => new Set([...previous, ...threadAncestors(threadState.threads, threadState.focusedId ?? -1).map(thread => thread.id)]));
      if (!dialog.open) restoreTarget = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      if (!dialog.open) dialog.showModal();
      const frame = requestAnimationFrame(() => {
        if (!dialog?.open) return;
        const selected = dialog.querySelector<HTMLButtonElement>(`[data-thread-row="${threadState.focusedId}"]`);
        (intent.kind === "create" ? dialog.querySelector<HTMLInputElement>("input") : selected ?? dialog.querySelector<HTMLButtonElement>("[data-thread-row]") ?? dialog.querySelector<HTMLInputElement>("input"))?.focus();
      });
      return () => cancelAnimationFrame(frame);
    } else if (dialog.open) {
      dialog.close();
      const destination = restoreTarget?.isConnected ? restoreTarget : document.querySelector<HTMLElement>('[data-slot="thread-navigation-trigger"]');
      destination?.focus();
    }
  });
  const tree = createMemo(() => threadTree(threadState.threads, section(), now(), expanded()));
  const visibleRows = createMemo(() => tree().map(row => ({ ...row, key: `tree:${row.thread.id}` })));
  const toggle = (id: number) => setExpanded(previous => { const next = new Set(previous); if (next.has(id)) next.delete(id); else next.add(id); return next; });
  let focusedRowIndex = 0;
  createEffect(() => visibleRows().map(row => row.key).join(","), () => {
    const frame = requestAnimationFrame(() => {
      if (!dialog?.open || document.activeElement !== document.body) return;
      const rows = dialog.querySelectorAll<HTMLButtonElement>("[data-thread-row]");
      (rows[Math.min(focusedRowIndex, rows.length - 1)] ?? dialog.querySelector<HTMLButtonElement>('[data-thread-filter]'))?.focus();
    });
    return () => cancelAnimationFrame(frame);
  });
  const move = (event: KeyboardEvent) => {
    const target = event.target as HTMLElement;
    if (!target.matches("[data-thread-row]")) return;
    const rows = Array.from(dialog?.querySelectorAll<HTMLButtonElement>("[data-thread-row]") ?? []);
    const current = rows.indexOf(target as HTMLButtonElement);
    const row = visibleRows()[current];
    if (row && (event.key === "ArrowRight" || event.key === "ArrowLeft")) {
      event.preventDefault();
      if (row.hasChildren && ((event.key === "ArrowRight" && !row.expanded) || (event.key === "ArrowLeft" && row.expanded))) toggle(row.thread.id);
      else if (event.key === "ArrowLeft" && row.thread.parent_thread_id !== null) dialog?.querySelector<HTMLButtonElement>(`[data-row-key="tree:${row.thread.parent_thread_id}"]`)?.focus();
      else if (event.key === "ArrowRight" && row.expanded) rows[current + 1]?.focus();
      return;
    }
    const next = event.key === "ArrowDown" ? Math.min(current + 1, rows.length - 1) : event.key === "ArrowUp" ? Math.max(0, current - 1) : event.key === "Home" ? 0 : event.key === "End" ? rows.length - 1 : null;
    if (next !== null) { event.preventDefault(); rows[next]?.focus(); }
  };
  const add = async (event: SubmitEvent) => {
    event.preventDefault();
    const value = title().trim();
    if (!value || creating()) return;
    setCreating(true); setError(null);
    try { const thread = await createThread(value, parentId()); setTitle(""); setSection("active"); props.onSelect(thread.id); }
    catch (cause) { setError(String(cause)); }
    finally { setCreating(false); }
  };
  const dismiss = (event: Event) => { event.preventDefault(); props.onClose(); };
  return <dialog ref={node => { dialog = node; }} id="thread-navigation" aria-label="Threads" data-slot="thread-drawer"
    onCancel={dismiss} onKeyDown={event => { if (event.key === "Escape") { event.stopPropagation(); dismiss(event); } }}
    onPointerDown={event => { if (event.target === dialog && dialog) { const rect = dialog.getBoundingClientRect(); if (event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom) props.onClose(); } }}
    class="fixed inset-y-0 left-14 m-0 h-dvh max-h-none w-[min(22rem,calc(100vw-3.5rem))] max-w-none flex-col border-0 border-r border-border bg-background p-4 text-foreground backdrop:bg-transparent open:flex">
    <div class="mb-4 flex items-center justify-between"><div class="min-w-0 flex-1"><h2 class="text-base font-semibold">Threads</h2><p data-slot="thread-filter-caption" class="text-xs capitalize text-muted-foreground">{section()}</p></div><DropdownMenu><DropdownMenuTrigger data-thread-filter class={`${control} ${section() !== "active" ? "bg-muted text-foreground" : ""}`} aria-label={`Filter threads: ${section()}`} title={`Filter threads: ${section()}`}><Funnel class="size-4" /></DropdownMenuTrigger><DropdownMenuContent><For each={["active", "settled", "snoozed", "archived"] as const}>{name => <DropdownMenuItem role="menuitemradio" aria-checked={section() === name ? "true" : "false"} class="min-h-11 capitalize" onSelect={() => setSection(name)}><Check class={section() === name ? "size-4" : "size-4 invisible"} />{name}</DropdownMenuItem>}</For></DropdownMenuContent></DropdownMenu><button class={control} aria-label="Search threads" title="Search threads" onClick={openThreadSearch}><Search class="size-4" /></button><button class={control} aria-label="Close threads" title="Close threads" onClick={props.onClose}><X class="size-5" /></button></div>
    <Show when={parentId() !== null}><p class="mb-2 break-words text-xs text-muted-foreground">New child in {threadPath(threadState.threads, parentId()!)} <button class="min-h-11 rounded px-2 underline" onClick={() => openThreadNavigation({ kind: "create", parentId: null })}>Create at top level</button></p></Show>
    <form onSubmit={event => void add(event)} class="mb-3 flex gap-2"><input aria-label="New thread title" placeholder={parentId() === null ? "New thread…" : "New child thread…"} value={title()} onInput={event => setTitle(event.currentTarget.value)} class="min-w-0 flex-1 rounded-lg border border-border bg-transparent px-3 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" /><button type="submit" class={`${control} bg-muted text-foreground`} aria-label="Create thread" title="Create thread" disabled={!title().trim() || creating() || state.connection !== "connected"}><Plus class="size-5" /></button></form>
    <Show when={error()}><div role="alert" class="mt-3 space-y-2 text-sm"><p>Couldn’t confirm the new thread. Your title is kept. Check the list before creating it again.</p><details><summary class="cursor-pointer py-2 text-muted-foreground">Technical details</summary><p class="break-words">{error()}</p></details></div></Show>

    <ThreadError navigation />
    <nav aria-label="Thread inventory" class="min-h-0 flex-1 overflow-y-auto" onFocusIn={event => { const row = (event.target as HTMLElement).closest('[data-thread-entry]'); if (row) focusedRowIndex = visibleRows().findIndex(item => item.key === row.getAttribute('data-thread-entry')); }}>
      <ul class="flex flex-col gap-1">
        <For each={visibleRows()} keyed={row => row.key}>{item => {
          const row = () => item();
          const thread = () => row().thread;
          const hasSignals = () => !thread().read || row().context || thread().attention === "needs_owner" || thread().running_turn !== null || thread().queued_turn_count !== 0 || thread().last_finished_turn !== null || thread().settled_at || thread().archived_at || (thread().snoozed_until && Date.parse(thread().snoozed_until!) > now());
          return <>
            <li data-thread-entry={row().key} data-context={row().context ? "true" : undefined} class={`relative flex shrink-0 items-start rounded-lg ${threadState.focusedId === thread().id ? "bg-muted/60" : "hover:bg-muted/30"}`} style={{ "margin-left": `${Math.min(row().depth, 4) * 16}px` }}>
              <Show when={row().depth > 0}><span aria-hidden="true" data-slot="thread-branch-guide" class="pointer-events-none absolute -top-1 bottom-0 left-1.5 w-3 border-l border-border/60"><span class="absolute top-[1.625rem] left-0 w-3 border-t border-border/60" /></span></Show>
              <Show when={row().hasChildren} fallback={<span aria-hidden="true" class="size-11 shrink-0" />}><button class={control} aria-label={`${row().expanded ? "Collapse" : "Expand"} ${thread().title}`} aria-expanded={row().expanded ? "true" : "false"} onClick={() => toggle(thread().id)}><ChevronRight class={`size-4 transition-transform ${row().expanded ? "rotate-90" : ""}`} /></button></Show>
              <button onContextMenu={event => { event.preventDefault(); event.currentTarget.parentElement?.querySelector<HTMLButtonElement>('[data-thread-actions]')?.click(); }} onKeyDown={event => { move(event); if (event.key === "ContextMenu" || (event.key === "F10" && event.shiftKey)) { event.preventDefault(); event.currentTarget.parentElement?.querySelector<HTMLButtonElement>('[data-thread-actions]')?.click(); } }} data-thread-row={thread().id} data-row-key={row().key} class="block min-h-11 min-w-0 flex-1 rounded-lg px-2 py-2 text-left text-sm text-muted-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring" aria-current={threadState.focusedId === thread().id ? "page" : undefined} title={threadPath(threadState.threads, thread().id)} onClick={() => props.onSelect(thread().id)}>
                <span class="flex items-center gap-2 font-medium text-foreground"><ThreadAvatar thread={thread()} /><span class="min-w-0 break-words">{thread().title}</span><Show when={thread().parent_thread_id === null && thread().pinned_at}><Pin class="size-3 shrink-0 text-muted-foreground" /><span class="sr-only">Pinned</span></Show></span>
                <span class="sr-only">Thread #{thread().id}</span>
                <Show when={hasSignals()}><span class="ml-9 mt-1 flex flex-wrap items-start gap-2 empty:hidden"><Show when={!thread().read}><span role="img" aria-label="Unread" title="Unread" class="mt-1 size-1.5 shrink-0 rounded-full bg-primary" /></Show><ThreadStatus thread={thread()} now={now()} compact /><Show when={row().context}><span class="text-xs">Parent context</span></Show></span></Show>
              </button><ThreadActions thread={thread()} quick now={now()} />
            </li>
          </>;
        }}</For>
      </ul>
      <Show when={tree().length === 0}><p class="p-3 text-sm text-muted-foreground">No {section()} threads</p></Show>
    </nav>
  </dialog>;
}
