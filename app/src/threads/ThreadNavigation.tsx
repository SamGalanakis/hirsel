import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { createOverlayPresence } from "../lib/focus";
import { Plus, X, Search, Funnel, Check, ChevronRight, Clock } from "../components/ui/icons";
import { type ThreadSection } from "./model";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "../components/ui/dropdown-menu";
import type { ThreadNavigationIntent } from "./navigation";
import type { Thread } from "./types";
import { ThreadAvatar } from "./ThreadAvatar";
import { ThreadError } from "./ThreadError";
import { ThreadActions } from "./ThreadActions";
import { ancestorsIn, pathIn, threadAncestors, threadIndex, threadTree } from "./tree";
import { openThreadCreate } from "./create";
import { threadRowSummary } from "./status";
import { state } from "../store/store";
import { historyId } from "../lib/history";
import { threadState } from "./store";

/** Dense inventory geometry. One line per Thread: a fine pointer reads 28px rows,
 * a coarse pointer keeps the 44px target DESIGN.md requires on phones. */
const ROW = "h-7 pointer-coarse:h-11";
const control = "inline-flex size-8 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-40 pointer-coarse:size-11";
const label = "px-2 pt-3 pb-1 text-meta font-medium uppercase tracking-wider text-muted-foreground";
/** Manual expand/collapse is the Owner's own choice, so it outlives a reload;
 * the depth default only fills the gaps it leaves. */
const expansionKey = (history: string | null) => `hirsel.thread-navigation.expanded.${history ?? "unknown"}`;
function readExpansion(history: string | null): Record<string, boolean> {
  try { const raw = localStorage.getItem(expansionKey(history)); const parsed: unknown = raw ? JSON.parse(raw) : null; return parsed && typeof parsed === "object" ? parsed as Record<string, boolean> : {}; }
  catch { return {}; }
}
function writeExpansion(history: string | null, choices: Record<string, boolean>): void {
  try { localStorage.setItem(expansionKey(history), JSON.stringify(choices)); }
  catch { /* The current session still expands when storage is unavailable. */ }
}
function Indicator(props: { thread: Thread; now: number }) {
  const summary = () => threadRowSummary(props.thread, props.now, state.connection === "connected");
  return <span aria-hidden="true" data-slot="thread-row-indicator" data-indicator={summary().indicator} class="pointer-events-none absolute -right-0.5 -bottom-0.5 inline-flex items-center justify-center rounded-full bg-inherit p-px empty:hidden">
    <Show when={summary().indicator === "running"}><span class="size-1.5 rounded-full bg-status-active motion-safe:animate-pulse" /></Show>
    <Show when={summary().indicator === "attention"}><span class="size-1.5 rounded-full bg-status-attention" /></Show>
    <Show when={summary().indicator === "queued"}><Clock class="size-2.5 text-muted-foreground" /></Show>
    <Show when={summary().indicator === "done"}><Check class="size-2.5 text-muted-foreground" /></Show>
  </span>;
}
export function ThreadNavigation(props: { open: boolean; modal: boolean; intent: ThreadNavigationIntent | null; onClose: () => void; onSelect: (id: number) => void }) {
  let dialog: HTMLDialogElement | undefined;
  let restoreTarget: HTMLElement | null = null;
  let displayedAsModal: boolean | null = null;
  let focusWasSummoned = false;
  let ownsFocus = false;
  const [section, setSection] = createSignal<ThreadSection>("active");
  const [choices, setChoices] = createSignal<Record<string, boolean>>({});
  const [query, setQuery] = createSignal("");
  const [now, setNow] = createSignal(Date.now());
  const trackFocus = (event: FocusEvent) => { ownsFocus = !!dialog?.contains(event.target as Node); };
  document.addEventListener("focusin", trackFocus);
  onCleanup(() => { document.removeEventListener("focusin", trackFocus); dialog?.close(); });
  createEffect(() => historyId(), history => { setChoices(readExpansion(history)); });
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
      if (!focusWasSummoned) restoreTarget = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      focusWasSummoned = true;
      const frame = requestAnimationFrame(() => {
        if (!dialog?.open) return;
        const selected = dialog.querySelector<HTMLElement>(`[data-thread-row="${threadState.focusedId}"]`);
        (selected ?? dialog.querySelector<HTMLElement>("[data-thread-row]") ?? dialog.querySelector<HTMLInputElement>("input"))?.focus();
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
  // Search keeps ancestry: a match stays reachable through the parents that hold it.
  const search = createMemo(() => {
    const needle = query().trim().toLocaleLowerCase();
    if (!needle) return null;
    const matches = threadState.threads.filter(thread => thread.title.toLocaleLowerCase().includes(needle));
    const keep = new Set(matches.map(thread => thread.id));
    const ancestors = new Set<number>();
    const index = threadIndex(threadState.threads);
    for (const match of matches) for (const parent of ancestorsIn(index, match.id)) { keep.add(parent.id); ancestors.add(parent.id); }
    return { keep, ancestors };
  });
  const selectedAncestry = createMemo(() => new Set(threadAncestors(threadState.threads, threadState.focusedId ?? -1).map(thread => thread.id)));
  const expandedFor = (thread: Thread, depth: number) => {
    if (search()?.ancestors.has(thread.id)) return true;
    const choice = choices()[String(thread.id)];
    if (choice !== undefined) return choice;
    if (selectedAncestry().has(thread.id)) return true;
    return depth <= 1;
  };
  const tree = createMemo(() => threadTree(threadState.threads, section(), now(), expandedFor));
  /** One index for the whole list: each row's ancestry tooltip is a walk, not a rebuild. */
  const rowIndex = createMemo(() => threadIndex(threadState.threads));
  const visibleRows = createMemo(() => tree().filter(row => !search() || search()!.keep.has(row.thread.id)).map(row => ({ ...row, key: `tree:${row.thread.id}` })));
  const sectionLabel = () => section() === "active" ? "Threads" : section() === "settled" ? "Done" : section();
  /** Pinned roots keep their own band; the rest sit under one quiet heading. */
  const listItems = createMemo(() => {
    const rows = visibleRows();
    const rooted = (row: typeof rows[number]) => row.depth === 0 && row.thread.parent_thread_id === null;
    const pinned = rows.some(row => rooted(row) && row.thread.pinned_at);
    const items: { key: string; label?: string; row?: typeof rows[number] }[] = [];
    let rest = false;
    for (const row of rows) {
      if (pinned && !items.length) items.push({ key: "label:pinned", label: "Pinned" });
      if (pinned && !rest && rooted(row) && !row.thread.pinned_at) { rest = true; items.push({ key: "label:rest", label: sectionLabel() }); }
      items.push({ key: row.key, row });
    }
    if (!pinned && rows.length && section() !== "active") items.unshift({ key: "label:rest", label: sectionLabel() });
    return items;
  });
  const toggle = (thread: Thread, depth: number) => {
    const next = { ...choices(), [String(thread.id)]: !expandedFor(thread, depth) };
    setChoices(next);
    writeExpansion(historyId(), next);
  };
  let focusedRowIndex = 0;
  createEffect(() => visibleRows().map(row => row.key).join(","), () => {
    const frame = requestAnimationFrame(() => {
      if (!dialog?.open || !ownsFocus || document.activeElement !== document.body) return;
      const rows = dialog.querySelectorAll<HTMLElement>("[data-thread-row]");
      (rows[Math.min(focusedRowIndex, rows.length - 1)] ?? dialog.querySelector<HTMLElement>('[data-thread-filter]'))?.focus();
    });
    return () => cancelAnimationFrame(frame);
  });
  const move = (event: KeyboardEvent) => {
    const target = event.target as HTMLElement;
    if (!target.matches("[data-thread-row]")) return;
    const rows = Array.from(dialog?.querySelectorAll<HTMLElement>("[data-thread-row]") ?? []);
    const current = rows.indexOf(target);
    const row = visibleRows()[current];
    if (row && (event.key === "ArrowRight" || event.key === "ArrowLeft")) {
      event.preventDefault();
      if (row.hasChildren && ((event.key === "ArrowRight" && !row.expanded) || (event.key === "ArrowLeft" && row.expanded))) toggle(row.thread, row.depth);
      else if (event.key === "ArrowLeft" && row.thread.parent_thread_id !== null) dialog?.querySelector<HTMLElement>(`[data-row-key="tree:${row.thread.parent_thread_id}"]`)?.focus();
      else if (event.key === "ArrowRight" && row.expanded) rows[current + 1]?.focus();
      return;
    }
    const next = event.key === "ArrowDown" ? Math.min(current + 1, rows.length - 1) : event.key === "ArrowUp" ? Math.max(0, current - 1) : event.key === "Home" ? 0 : event.key === "End" ? rows.length - 1 : null;
    if (next !== null) { event.preventDefault(); rows[next]?.focus(); }
  };
  const dismiss = (event: Event) => { event.preventDefault(); props.onClose(); };
  return <dialog ref={node => { dialog = node; }} id="thread-navigation" role={props.modal ? "dialog" : "complementary"} aria-modal={props.modal ? "true" : undefined} aria-label="Spaces and Tasks" data-slot="thread-drawer"
    onCancel={dismiss} onKeyDown={event => { if (event.key === "Escape") { event.stopPropagation(); dismiss(event); } }}
    onPointerDown={event => { if (event.target === dialog && dialog) { const rect = dialog.getBoundingClientRect(); if (event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom) props.onClose(); } }}
    class="fixed inset-y-0 left-14 m-0 h-dvh max-h-none w-[min(20rem,calc(100vw-3.5rem))] max-w-none flex-col border-0 border-r border-border bg-background p-2 text-foreground backdrop:bg-transparent open:flex workspace:static workspace:z-auto workspace:h-auto workspace:w-72 workspace:shrink-0">
    <header class="mb-1 flex flex-col gap-1">
      <div data-slot="thread-drawer-identity" class="flex min-w-0 items-center gap-0.5">
        <h2 class="min-w-0 flex-1 truncate px-1 text-sm font-semibold">Spaces &amp; Tasks</h2>
        <div data-slot="thread-drawer-actions" class="flex shrink-0 items-center">
          <button class={control} aria-label="New Space or Task" title="New Space or Task" onClick={() => openThreadCreate(null)}><Plus class="size-4" /></button>
          <DropdownMenu><DropdownMenuTrigger data-thread-filter class={`${control} ${section() !== "active" ? "bg-muted text-foreground" : ""}`} aria-label={`Filter work: ${section() === "settled" ? "done" : section()}`} title={`Filter work: ${section() === "settled" ? "done" : section()}`}><Funnel class="size-4" /></DropdownMenuTrigger><DropdownMenuContent><For each={["active", "settled", "snoozed", "archived"] as const}>{name => <DropdownMenuItem role="menuitemradio" aria-checked={section() === name ? "true" : "false"} class="min-h-11 capitalize" onSelect={() => setSection(name)}><Check class={section() === name ? "size-4" : "size-4 invisible"} />{name === "settled" ? "done" : name}</DropdownMenuItem>}</For></DropdownMenuContent></DropdownMenu>
          <button class={control} aria-label="Close Spaces and Tasks" title="Close Spaces and Tasks" onClick={props.onClose}><X class="size-4" /></button>
        </div>
      </div>
      <div class="relative flex items-center">
        <Search aria-hidden="true" class="pointer-events-none absolute left-2 size-3.5 text-muted-foreground" />
        <input type="search" aria-label="Search Spaces and Tasks" placeholder="Search…" value={query()} onInput={event => setQuery(event.currentTarget.value)}
          class="h-7 w-full min-w-0 rounded-md border border-border bg-transparent pl-7 pr-2 text-sm placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:h-11" />
      </div>
    </header>

    <ThreadError navigation />
    <nav aria-label="Thread inventory" class="min-h-0 flex-1 overflow-y-auto" onFocusIn={event => { const row = (event.target as HTMLElement).closest('[data-thread-entry]'); if (row) focusedRowIndex = visibleRows().findIndex(item => item.key === row.getAttribute('data-thread-entry')); }}>
      <ul role="tree" aria-label="Threads" class="flex flex-col">
        <For each={listItems()} keyed={item => item.key}>{entry => {
          const item = () => entry();
          return <Show when={item().row} fallback={<li role="presentation" class={label}>{item().label}</li>}>{row => {
            const thread = () => row().thread;
            const summary = () => threadRowSummary(thread(), now(), state.connection === "connected");
            const done = () => thread().kind === "task" && !!thread().settled_at;
            const describe = () => [`${thread().kind === "space" ? "Space" : "Task"} ${thread().title} #${thread().id}`, thread().parent_thread_id === null && thread().pinned_at ? "Pinned" : null, thread().read ? null : "Unread", row().context ? "Parent context" : null, summary().sentence || null].filter(Boolean).join(" · ");
            return <>
              <li data-thread-entry={row().key} data-thread-row={thread().id} data-row-key={row().key} data-context={row().context ? "true" : undefined}
                role="treeitem" tabindex="0" aria-level={row().depth + 1} aria-selected={threadState.focusedId === thread().id ? "true" : "false"} aria-expanded={row().hasChildren ? (row().expanded ? "true" : "false") : undefined}
                aria-current={threadState.focusedId === thread().id ? "page" : undefined} aria-label={describe()} title={`${pathIn(rowIndex(), thread().id)}${summary().sentence ? ` — ${summary().sentence}` : ""}`}
                class={`group relative flex ${ROW} shrink-0 cursor-default items-center gap-1.5 rounded-md pr-0.5 text-sm focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring ${threadState.focusedId === thread().id ? "bg-muted text-foreground before:absolute before:inset-y-1 before:left-0 before:w-0.5 before:rounded-full before:bg-primary" : "bg-background text-muted-foreground hover:bg-muted"}`}
                style={{ "padding-left": `${Math.min(row().depth, 6) * 12}px` }}
                onClick={event => { if (!(event.target as HTMLElement).closest("button")) props.onSelect(thread().id); }}
                onContextMenu={event => { event.preventDefault(); event.currentTarget.querySelector<HTMLButtonElement>('[data-thread-actions]')?.click(); }}
                onKeyDown={event => {
                  move(event);
                  if (event.target !== event.currentTarget) return;
                  if (event.key === "Enter" || event.key === " ") { event.preventDefault(); props.onSelect(thread().id); }
                  if (event.key === "ContextMenu" || (event.key === "F10" && event.shiftKey)) { event.preventDefault(); event.currentTarget.querySelector<HTMLButtonElement>('[data-thread-actions]')?.click(); }
                }}>
                <For each={Array.from({ length: Math.min(row().depth, 6) }, (_, level) => level)}>{level => <span aria-hidden="true" data-slot="thread-branch-guide" class="pointer-events-none absolute inset-y-0 w-px bg-border/60" style={{ left: `${level * 12 + 7}px` }} />}</For>
                <Show when={row().hasChildren} fallback={<span aria-hidden="true" class="w-4 shrink-0" />}>
                  <button type="button" class="relative inline-flex w-4 shrink-0 items-center justify-center self-stretch rounded-sm text-muted-foreground hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring pointer-coarse:before:absolute pointer-coarse:before:inset-y-0 pointer-coarse:before:-inset-x-2" tabindex="-1" aria-label={`${row().expanded ? "Collapse" : "Expand"} ${thread().title}`} onClick={() => toggle(thread(), row().depth)}>
                    <ChevronRight class={`size-3 transition-transform ${row().expanded ? "rotate-90" : ""}`} />
                  </button>
                </Show>
                <span class="relative shrink-0 bg-inherit">
                  <Show when={!done()} fallback={<span aria-hidden="true" class="inline-flex size-4 items-center justify-center rounded-full text-muted-foreground"><Check class="size-3.5" /></span>}>
                    <ThreadAvatar thread={thread()} dense />
                  </Show>
                  <Indicator thread={thread()} now={now()} />
                </span>
                <span data-slot="thread-row-title" class={`min-w-0 flex-1 truncate ${done() ? "text-muted-foreground" : thread().read ? "text-foreground" : "font-medium text-foreground"}`}>{thread().title}</span>
                <Show when={!thread().read}><span role="img" aria-label="Unread" title="Unread" class="size-1.5 shrink-0 rounded-full bg-primary" /></Show>
                <Show when={summary().meta}>{meta => <span data-slot="thread-row-meta" aria-hidden="true" class={`shrink-0 whitespace-nowrap text-meta tabular-nums ${summary().indicator === "running" ? "text-status-active" : "text-muted-foreground"}`}>{meta()}</span>}</Show>
                <span data-slot="thread-row-actions" class="absolute right-0.5 flex items-center rounded-md bg-inherit opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100 pointer-coarse:static pointer-coarse:opacity-100">
                  <ThreadActions thread={thread()} quick dense now={now()} />
                </span>
              </li>
            </>;
          }}</Show>;
        }}</For>
      </ul>
      <Show when={visibleRows().length === 0}><p class="p-3 text-sm text-muted-foreground">{search() ? `No matches for “${query().trim()}”` : `No ${section() === "settled" ? "done Tasks" : `${section()} Spaces or Tasks`}`}</p></Show>
    </nav>
  </dialog>;
}
