// The queue filter bar (owner addendum, supersedes the quiet Archived toggle): a
// calm search + a segmented Active · Needs you · Snoozed(n) · Archived(n) control
// that sits on top of the queue surface (the desktop Feed header, the phone
// peek), with the quiet "Clear finished (n)" sweep at its trailing end. Hairline,
// 13px, nothing red — it must read as chrome the cards sit under, never compete
// with them. Default is Active (the control's mode/search live in shared signals
// so ⌘K can drive them; the phone peek resets them to Active/empty on open, so
// archived/snoozed events stay hidden until asked for).
import { Search, X } from "lucide-solid";
import { createSignal, For, Show } from "solid-js";
import type { EventItem } from "../../protocol";
import { cn } from "@/lib/utils";
import { eventTitle } from "../../store/selectors";

/** The four filters. `active` is the resting queue (non-archived, non-snoozed —
 * THE default), `needs-you` narrows to open judgments (the one-red set),
 * `snoozed` discloses the parked events (with Unsnooze + return time on each, and
 * only when any exist), and `archived` discloses the swept-away day-log (with
 * Unarchive on each). */
export type QueueFilterMode = "active" | "needs-you" | "snoozed" | "archived";

/** Shared filter state (Wave-3): lifted out of the individual surfaces so the ⌘K
 * palette's filter switches + "Search events" can drive the same control the
 * Feed/peek render. The desktop Feed column reads these directly; the phone peek
 * resets them to Active/empty on open (it unmounts on close), preserving its
 * fresh-open behavior. */
export const [queueFilterMode, setQueueFilterMode] = createSignal<QueueFilterMode>("active");
export const [queueSearch, setQueueSearch] = createSignal("");

/** Live text match for the search box: case-insensitive substring across the
 * @handle, the one-line description, and the derived card title. Empty query
 * matches everything (the search never narrows until the Owner types). One
 * matcher so every surface filters identically. */
export function matchesQuery(ev: EventItem, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (q.length === 0) return true;
  return `${ev.name} ${ev.description} ${eventTitle(ev)}`.toLowerCase().includes(q);
}

export function QueueFilterBar(props: {
  query: string;
  onQueryChange: (q: string) => void;
  mode: QueueFilterMode;
  onModeChange: (m: QueueFilterMode) => void;
  archivedCount: number;
  /** Wave-3: number of parked (snoozed) events — the "Snoozed(n)" chip appears
   * ONLY when > 0. */
  snoozedCount?: number;
  /** Wave-3: number of finished events the sweep would clear — the quiet "Clear
   * finished (n)" trailing action appears only when > 0 and a handler is given. */
  finishedCount?: number;
  onClearFinished?: () => void;
  class?: string;
}) {
  // Active · Needs you are always present; Snoozed/Archived disclose only when
  // they hold something, so the control never offers an empty parking lot.
  const modes = (): { value: QueueFilterMode; label: string; count?: number }[] => [
    { value: "active", label: "Active" },
    { value: "needs-you", label: "Needs you" },
    ...((props.snoozedCount ?? 0) > 0
      ? [{ value: "snoozed" as const, label: "Snoozed", count: props.snoozedCount }]
      : []),
    ...(props.archivedCount > 0
      ? [{ value: "archived" as const, label: "Archived", count: props.archivedCount }]
      : []),
  ];
  return (
    <div data-slot="queue-filter" class={cn("flex items-center gap-2", props.class)}>
      <div class="relative flex min-w-0 flex-1 items-center">
        <Search
          class="pointer-events-none absolute left-2 size-3.5 text-muted-foreground/70"
          aria-hidden="true"
        />
        <input
          data-slot="queue-search"
          type="search"
          value={props.query}
          onInput={(e) => props.onQueryChange(e.currentTarget.value)}
          placeholder="Search the queue"
          aria-label="Search the queue"
          class="h-8 w-full rounded-md border border-input bg-transparent pl-7 pr-7 text-[0.8125rem] text-foreground shadow-xs outline-none placeholder:text-muted-foreground/70 focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/40 [&::-webkit-search-cancel-button]:appearance-none"
        />
        <Show when={props.query.length > 0}>
          <button
            type="button"
            aria-label="Clear search"
            class="absolute right-1.5 grid size-5 place-items-center rounded text-muted-foreground/70 transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50 active:translate-y-px"
            onClick={() => props.onQueryChange("")}
          >
            <X class="size-3.5" aria-hidden="true" />
          </button>
        </Show>
      </div>
      <div
        role="group"
        aria-label="Filter events"
        class="flex shrink-0 items-center gap-0.5 rounded-md border border-border bg-muted/40 p-0.5"
      >
        <For each={modes()}>
          {(m) => (
            <button
              type="button"
              data-slot="queue-filter-chip"
              data-mode={m.value}
              aria-pressed={props.mode === m.value}
              class={cn(
                "inline-flex items-center rounded-[5px] px-2 py-1 text-[0.75rem] font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50 active:translate-y-px",
                props.mode === m.value
                  ? "bg-card text-foreground shadow-xs"
                  : "text-muted-foreground hover:text-foreground",
              )}
              onClick={() => props.onModeChange(m.value)}
            >
              {m.label}
              <Show when={m.count !== undefined && m.count > 0}>
                <span class="ml-1 tabular-nums text-muted-foreground/70">{m.count}</span>
              </Show>
            </button>
          )}
        </For>
      </div>
      {/* The quiet sweep — a demoted link-style action at the trailing end of the
          row, no red, no fill. Only when something is finished to clear. */}
      <Show when={props.onClearFinished && (props.finishedCount ?? 0) > 0}>
        <button
          type="button"
          data-slot="clear-finished"
          class="shrink-0 whitespace-nowrap rounded-sm px-1 text-[0.75rem] font-medium text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50 active:translate-y-px"
          onClick={() => props.onClearFinished?.()}
        >
          Clear finished <span class="tabular-nums">{props.finishedCount}</span>
        </button>
      </Show>
    </div>
  );
}
