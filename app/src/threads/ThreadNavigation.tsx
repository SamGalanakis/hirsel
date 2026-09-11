import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { createOverlayPresence } from "../lib/focus";
import { Plus, X, Search, Funnel, Check, ChevronRight, Pin } from "../components/ui/icons";
import { type ThreadSection } from "./model";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "../components/ui/dropdown-menu";
import { openThreadSearch } from "../lib/keymap";
import type { ThreadNavigationIntent } from "./navigation";
import type { ThreadKind } from "./types";
import { ThreadAvatar } from "./ThreadAvatar";
import { ThreadStatus } from "./ThreadStatus";
import { ThreadError } from "./ThreadError";
import { ThreadActions } from "./ThreadActions";
import { threadAncestors, threadPath, threadTree } from "./tree";
import { openThreadNavigation } from "./navigation";
import { state } from "../store/store";
import { historyId } from "../lib/history";
import { createThread, threadState } from "./store";

const control = "inline-flex size-11 shrink-0 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-40";
export function ThreadNavigation(props: { open: boolean; modal: boolean; intent: ThreadNavigationIntent | null; onClose: () => void; onSelect: (id: number) => void }) {
  let dialog: HTMLDialogElement | undefined;
  let restoreTarget: HTMLElement | null = null;
  let displayedAsModal: boolean | null = null;
  let focusWasSummoned = false;
  let ownsFocus = false;
  const [section, setSection] = createSignal<ThreadSection>("active");
  const [expanded, setExpanded] = createSignal<ReadonlySet<number>>(new Set());
  const [parentId, setParentId] = createSignal<number | null>(null);
  const [createHistoryId, setCreateHistoryId] = createSignal<string | null>(null);
  const [title, setTitle] = createSignal("");
  const [creating, setCreating] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [now, setNow] = createSignal(Date.now());
  const trackFocus = (event: FocusEvent) => { ownsFocus = !!dialog?.contains(event.target as Node); };
  document.addEventListener("focusin", trackFocus);
  onCleanup(() => { document.removeEventListener("focusin", trackFocus); dialog?.close(); });
  createEffect(() => props.open, open => {
    if (!open) return;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(timer);
  });
  createOverlayPresence(() => props.open && props.modal);
  createEffect(() => ({ open: props.open, modal: props.modal }), next => {
    if (!dialog) return;
    if (!next.open) {
      if (dialog.open) dialog.close();
      displayedAsModal = null;
      return;
    }
    if (dialog.open && displayedAsModal !== next.modal) dialog.close();
    if (!dialog.open) {
      if (next.modal) {
        // Capture the opener before showModal runs the browser's native focus
        // steps. The intent effect will then focus the requested row/input,
        // while dismissal can still return to the control outside the drawer.
        if (props.intent && !focusWasSummoned) {
          restoreTarget = document.activeElement instanceof HTMLElement ? document.activeElement : null;
          focusWasSummoned = true;
        }
        dialog.showModal();
      }
      // Setting the non-modal state directly avoids the browser's dialog
      // focusing steps. A default wide dock is standing navigation, so only an
      // explicit opening intent below should move focus into it.
      else dialog.setAttribute("open", "");
    }
    displayedAsModal = next.modal;
  });
  createEffect(() => props.intent, intent => {
    if (!dialog) return;
    if (intent) {
      setParentId(intent.kind === "create" ? intent.parentId : null);
      setCreateHistoryId(intent.kind === "create" ? intent.historyId : null);
      setExpanded(previous => new Set([...previous, ...threadAncestors(threadState.threads, threadState.focusedId ?? -1).map(thread => thread.id)]));
      if (!focusWasSummoned) restoreTarget = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      focusWasSummoned = true;
      const frame = requestAnimationFrame(() => {
        if (!dialog?.open) return;
        const selected = dialog.querySelector<HTMLButtonElement>(`[data-thread-row="${threadState.focusedId}"]`);
        (intent.kind === "create" ? dialog.querySelector<HTMLInputElement>("input") : selected ?? dialog.querySelector<HTMLButtonElement>("[data-thread-row]") ?? dialog.querySelector<HTMLInputElement>("input"))?.focus();
      });
      return () => cancelAnimationFrame(frame);
    } else if (focusWasSummoned) {
      focusWasSummoned = false;
      if (!props.open) {
        const destination = restoreTarget?.isConnected ? restoreTarget : document.querySelector<HTMLElement>('[data-slot="thread-navigation-trigger"]');
        destination?.focus();
      }
    }
  });
  const tree = createMemo(() => threadTree(threadState.threads, section(), now(), expanded()));
  const visibleRows = createMemo(() => tree().map(row => ({ ...row, key: `tree:${row.thread.id}` })));
  const toggle = (id: number) => setExpanded(previous => { const next = new Set(previous); if (next.has(id)) next.delete(id); else next.add(id); return next; });
  let focusedRowIndex = 0;
  createEffect(() => visibleRows().map(row => row.key).join(","), () => {
    const frame = requestAnimationFrame(() => {
      if (!dialog?.open || !ownsFocus || document.activeElement !== document.body) return;
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
  const parent = createMemo(() => threadState.threads.find(thread => thread.id === parentId()));
  const canCreateSpace = () => parentId() === null || parent()?.kind === "space";
  const add = async (event: SubmitEvent) => {
    event.preventDefault();
    const value = title().trim();
    if (!value || creating()) return;
    setCreating(true); setError(null);
    const expectedHistory = createHistoryId() ?? historyId();
    if (!expectedHistory) { setCreating(false); setError("History is unavailable. Reconnect and try again."); return; }
    const kind = ((event.submitter as HTMLButtonElement | null)?.value ?? "task") as ThreadKind;
    if (kind === "space" && !canCreateSpace()) { setCreating(false); setError("Tasks can contain child Tasks only."); return; }
    try { const thread = await createThread(expectedHistory, value, kind, parentId()); setTitle(""); setSection("active"); props.onSelect(thread.id); }
    catch (cause) { setError(String(cause)); }
    finally { setCreating(false); }
  };
  const dismiss = (event: Event) => { event.preventDefault(); props.onClose(); };
  return <dialog ref={node => { dialog = node; }} id="thread-navigation" role={props.modal ? "dialog" : "complementary"} aria-modal={props.modal ? "true" : undefined} aria-label="Spaces and Tasks" data-slot="thread-drawer"
    onCancel={dismiss} onKeyDown={event => { if (event.key === "Escape") { event.stopPropagation(); dismiss(event); } }}
    onPointerDown={event => { if (event.target === dialog && dialog) { const rect = dialog.getBoundingClientRect(); if (event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom) props.onClose(); } }}
    class="fixed inset-y-0 left-14 m-0 h-dvh max-h-none w-[min(22rem,calc(100vw-3.5rem))] max-w-none flex-col border-0 border-r border-border bg-background p-4 text-foreground backdrop:bg-transparent open:flex workspace:static workspace:z-auto workspace:h-auto workspace:w-80 workspace:shrink-0">
    <div class="mb-4 flex items-center justify-between"><div class="min-w-0 flex-1"><h2 class="text-base font-semibold">Spaces & Tasks</h2><p data-slot="thread-filter-caption" class="text-xs capitalize text-muted-foreground">{section() === "settled" ? "done" : section()}</p></div><DropdownMenu><DropdownMenuTrigger data-thread-filter class={`${control} ${section() !== "active" ? "bg-muted text-foreground" : ""}`} aria-label={`Filter work: ${section() === "settled" ? "done" : section()}`} title={`Filter work: ${section() === "settled" ? "done" : section()}`}><Funnel class="size-4" /></DropdownMenuTrigger><DropdownMenuContent><For each={["active", "settled", "snoozed", "archived"] as const}>{name => <DropdownMenuItem role="menuitemradio" aria-checked={section() === name ? "true" : "false"} class="min-h-11 capitalize" onSelect={() => setSection(name)}><Check class={section() === name ? "size-4" : "size-4 invisible"} />{name === "settled" ? "done" : name}</DropdownMenuItem>}</For></DropdownMenuContent></DropdownMenu><button class={control} aria-label="Search spaces and tasks" title="Search spaces and tasks" onClick={openThreadSearch}><Search class="size-4" /></button><button class={control} aria-label="Close Spaces and Tasks" title="Close Spaces and Tasks" onClick={props.onClose}><X class="size-5" /></button></div>
    <Show when={parentId() !== null}><p class="mb-2 break-words text-xs text-muted-foreground">New child in {threadPath(threadState.threads, parentId()!)} <button class="min-h-11 rounded px-2 underline" onClick={() => { const history = historyId(); if (history) openThreadNavigation({ kind: "create", historyId: history, parentId: null }); }}>Create at top level</button></p></Show>
    <form onSubmit={event => void add(event)} class="mb-3 space-y-2"><input aria-label="New space or task title" placeholder={parentId() === null ? "Name this Space or Task…" : parent()?.kind === "task" ? "Name this Task…" : "Name this child…"} value={title()} onInput={event => setTitle(event.currentTarget.value)} class="h-11 w-full min-w-0 rounded-lg border border-border bg-transparent px-3 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" /><div class="grid grid-cols-2 gap-2"><Show when={canCreateSpace()}><button type="submit" name="kind" value="space" class={`${control} size-auto bg-muted px-3 text-foreground`} disabled={!title().trim() || creating() || state.connection !== "connected"}><Plus class="size-4" />New Space</button></Show><button type="submit" name="kind" value="task" class={`${control} size-auto bg-muted px-3 text-foreground ${canCreateSpace() ? "" : "col-span-2"}`} disabled={!title().trim() || creating() || state.connection !== "connected"}><Plus class="size-4" />New Task</button></div></form>
    <Show when={error()}><div role="alert" class="mt-3 space-y-2 text-sm"><p>Couldn’t confirm the new item. Your title is kept. Check the list before creating it again.</p><details><summary class="cursor-pointer py-2 text-muted-foreground">Technical details</summary><p class="break-words">{error()}</p></details></div></Show>

    <ThreadError navigation />
    <nav aria-label="Thread inventory" class="min-h-0 flex-1 overflow-y-auto" onFocusIn={event => { const row = (event.target as HTMLElement).closest('[data-thread-entry]'); if (row) focusedRowIndex = visibleRows().findIndex(item => item.key === row.getAttribute('data-thread-entry')); }}>
      <ul class="flex flex-col gap-1">
        <For each={visibleRows()} keyed={row => row.key}>{item => {
          const row = () => item();
          const thread = () => row().thread;
          const hasSignals = () => !thread().read || row().context || thread().attention === "needs_owner" || thread().running_turn !== null || thread().queued_turn_count !== 0 || thread().last_finished_turn !== null || thread().settled_at || thread().archived_at || (thread().snoozed_until && Date.parse(thread().snoozed_until!) > now());
          return <>
            <li data-thread-entry={row().key} data-context={row().context ? "true" : undefined} class={`relative flex shrink-0 items-start rounded-lg ${threadState.focusedId === thread().id ? "bg-muted/60" : "hover:bg-muted/30"}`} style={{ "margin-left": `clamp(0px, calc(100% - 13.5rem), ${Math.min(row().depth, 4) * 12}px)` }}>
              <Show when={row().depth > 0}><span aria-hidden="true" data-slot="thread-branch-guide" class="pointer-events-none absolute -top-1 bottom-0 left-1.5 w-3 border-l border-border/60"><span class="absolute top-[1.625rem] left-0 w-3 border-t border-border/60" /></span></Show>
              <Show when={row().hasChildren} fallback={<span aria-hidden="true" class="absolute left-0 top-0 size-11" />}><button class={`${control} absolute left-0 top-0 z-10`} aria-label={`${row().expanded ? "Collapse" : "Expand"} ${thread().title}`} aria-expanded={row().expanded ? "true" : "false"} onClick={() => toggle(thread().id)}><ChevronRight class={`size-4 transition-transform ${row().expanded ? "rotate-90" : ""}`} /></button></Show>
              <button onContextMenu={event => { event.preventDefault(); event.currentTarget.parentElement?.querySelector<HTMLButtonElement>('[data-thread-actions]')?.click(); }} onKeyDown={event => { move(event); if (event.key === "ContextMenu" || (event.key === "F10" && event.shiftKey)) { event.preventDefault(); event.currentTarget.parentElement?.querySelector<HTMLButtonElement>('[data-thread-actions]')?.click(); } }} data-thread-row={thread().id} data-row-key={row().key} class="block min-h-11 min-w-0 flex-1 rounded-lg px-2 py-2 text-left text-sm text-muted-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring" aria-current={threadState.focusedId === thread().id ? "page" : undefined} title={threadPath(threadState.threads, thread().id)} onClick={() => props.onSelect(thread().id)}>
                <span class="flex min-h-8 items-center gap-2 pl-11 pr-11 font-medium text-foreground min-[420px]:pr-[5.5rem]"><ThreadAvatar thread={thread()} /><span class="min-w-0 flex-1"><span data-slot="thread-row-title" class="block break-words">{thread().title}</span><span class="mt-0.5 flex items-center gap-1.5 text-xs font-normal capitalize text-muted-foreground"><span>{thread().kind}</span><Show when={thread().parent_thread_id === null && thread().pinned_at}><Pin class="size-3 shrink-0" /><span class="sr-only">Pinned</span></Show></span></span></span>
                <span class="sr-only">Thread #{thread().id}</span>
                <Show when={hasSignals()}><span data-slot="thread-row-signals" class="mt-1 flex flex-wrap items-start gap-2 empty:hidden"><Show when={!thread().read}><span role="img" aria-label="Unread" title="Unread" class="mt-1 size-1.5 shrink-0 rounded-full bg-primary" /></Show><ThreadStatus thread={thread()} now={now()} compact /><Show when={row().context}><span class="text-xs">Parent context</span></Show></span></Show>
              </button><span class="absolute right-0 top-0 z-10"><ThreadActions thread={thread()} quick now={now()} /></span>
            </li>
          </>;
        }}</For>
      </ul>
      <Show when={tree().length === 0}><p class="p-3 text-sm text-muted-foreground">No {section() === "settled" ? "done Tasks" : `${section()} Spaces or Tasks`}</p></Show>
    </nav>
  </dialog>;
}
