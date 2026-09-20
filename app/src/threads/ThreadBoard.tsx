import { createEffect, createMemo, createSignal, For, Show, untrack } from "solid-js";
import { SectionLabel } from "../components/ui/section-label";
import { historyId } from "../lib/history";
import { focusThread, markHeadlinesSeen, threadState } from "./store";
import { topLevelSpaceForThread } from "../spaces/store";
import type { Thread } from "./types";
import type { ViewSpec } from "../protocol";
import { attentionExcerpt } from "./attention";

const statusLabel = (thread: Thread) => thread.status.kind.replace("needs_you", "Needs you").replace(/^./, value => value.toUpperCase());
type Band = "needs" | "changed" | "rest";
interface Placement { id: number; band: Band }
const band = (thread: Thread): Band => thread.status.kind === "needs_you" && thread.settled_at === null ? "needs" : thread.headline_revision > thread.last_seen_headline_revision ? "changed" : "rest";
const ordered = (rows: Thread[]) => [...rows].sort((left, right) => {
  const rank = (thread: Thread) => ({ needs: 0, changed: 1, rest: 2 })[band(thread)];
  return rank(left) - rank(right) || left.id - right.id;
});

function instrumentText(spec: ViewSpec | ViewSpec[] | null): string {
  const candidates: string[] = [];
  const visit = (value: unknown) => {
    if (Array.isArray(value)) { value.forEach(visit); return; }
    if (value === null || typeof value !== "object") return;
    const node = value as Record<string, unknown>;
    if ((node.type === "heading" || node.type === "text") && typeof node.text === "string") candidates.push(node.text);
    Object.values(node).forEach(visit);
  };
  visit(spec);
  return attentionExcerpt(candidates[0]);
}

export function ThreadBoard(props: { spaceId: number; visible: () => boolean }) {
  const live = createMemo(() => ordered(threadState.threads.filter(thread => !thread.archived_at && topLevelSpaceForThread(threadState.threads, thread.id)?.id === props.spaceId)));
  const livePlacements = createMemo<Placement[]>(() => live().map(thread => ({ id: thread.id, band: band(thread) })));
  const [frozen, setFrozen] = createSignal<Placement[] | null>(null);
  const [hovering, setHovering] = createSignal(false);
  const [focused, setFocused] = createSignal(false);
  const placements = () => frozen() ?? livePlacements();
  const freeze = () => setFrozen(current => current ?? livePlacements());
  const rows = (kind: Band) => placements().filter(row => row.band === kind).flatMap(row => {
    const thread = threadState.threads.find(item => item.id === row.id);
    return thread ? [thread] : [];
  });
  createEffect(() => props.visible() ? ({ currentHistory: historyId(), ids: live().filter(thread => thread.headline_revision > thread.last_seen_headline_revision).map(thread => thread.id) }) : null, target => {
    if (!target?.currentHistory || !target.ids.length) return;
    const currentHistory = target.currentHistory;
    untrack(() => markHeadlinesSeen(currentHistory, target.ids));
  });
  const bands = createMemo(() => [
    { id: "needs" as const, label: "Needs you", rows: rows("needs") },
    { id: "changed" as const, label: "Changed since you looked", rows: rows("changed") },
    { id: "rest" as const, label: "Everything else", rows: rows("rest") },
  ]);
  return <aside class="flex min-h-0 min-w-0 flex-1 flex-col overflow-y-auto rounded-xl border border-border bg-background p-3" aria-label="Space board" onPointerEnter={() => { setHovering(true); freeze(); }} onPointerLeave={() => { setHovering(false); if (!focused()) setFrozen(null); }} onFocusIn={() => { setFocused(true); freeze(); }} onFocusOut={event => { if ((event.currentTarget as HTMLElement).contains(event.relatedTarget as Node | null)) return; setFocused(false); if (!hovering()) setFrozen(null); }}>
    <div class="mb-3"><h2 class="text-base font-medium">Board</h2><p class="text-xs text-muted-foreground">Headlines and current Host status</p></div>
    <For each={bands()}>{group => <Show when={group.rows.length > 0}><section class="mb-4"><SectionLabel as="h3" class="mb-1 px-2">{group.label}</SectionLabel><ul class="space-y-1"><For each={group.rows}>{thread => <li><button data-board-thread={thread.id} class="min-h-11 w-full rounded-lg px-3 py-2 text-left hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" onClick={() => focusThread(thread.id)}><span class="flex items-baseline justify-between gap-3"><span class="text-sm font-medium">#{thread.id} {thread.title}</span><span class="shrink-0 text-xs text-muted-foreground">{statusLabel(thread)}</span></span><Show when={group.id === "needs" && instrumentText(thread.instrument)} fallback={<Show when={group.id === "changed" && thread.previous_headline} fallback={<span class="block text-sm text-muted-foreground">{thread.headline}</span>}>{previous => <span class="block text-sm text-muted-foreground">{previous()} → {thread.headline}</span>}</Show>}>{question => <span class="block text-sm text-muted-foreground">{question()}</span>}</Show><span class="block text-xs text-muted-foreground">{thread.status.reason}</span></button></li>}</For></ul></section></Show>}</For>
  </aside>;
}
