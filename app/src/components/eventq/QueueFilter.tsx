// The queue filter bar (owner addendum, supersedes the quiet Archived toggle): a
// calm search + a segmented Active · Needs you · Archived(n) control that sits on
// top of the queue surface (the desktop Feed header, the phone peek). Hairline,
// 13px, nothing red — it must read as chrome the cards sit under, never compete
// with them. Default is Active every session (the control is session-local
// signal state, never persisted), so archived events stay hidden until asked for.
import { Search, X } from "lucide-solid";
import { For, Show } from "solid-js";
import type { EventItem } from "../../protocol";
import { cn } from "@/lib/utils";
import { eventTitle } from "../../store/selectors";

/** The three filters. `active` is the resting queue (everything non-archived —
 * THE default), `needs-you` narrows to open judgments (the one-red set), and
 * `archived` discloses the swept-away events (with Unarchive on each). */
export type QueueFilterMode = "active" | "needs-you" | "archived";

const MODES: { value: QueueFilterMode; label: string }[] = [
  { value: "active", label: "Active" },
  { value: "needs-you", label: "Needs you" },
  { value: "archived", label: "Archived" },
];

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
  class?: string;
}) {
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
            class="absolute right-1.5 grid size-5 place-items-center rounded text-muted-foreground/70 transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
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
        <For each={MODES}>
          {(m) => (
            <button
              type="button"
              data-slot="queue-filter-chip"
              data-mode={m.value}
              aria-pressed={props.mode === m.value}
              class={cn(
                "inline-flex items-center rounded-[5px] px-2 py-1 text-[0.75rem] font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50",
                props.mode === m.value
                  ? "bg-card text-foreground shadow-xs"
                  : "text-muted-foreground hover:text-foreground",
              )}
              onClick={() => props.onModeChange(m.value)}
            >
              {m.label}
              <Show when={m.value === "archived" && props.archivedCount > 0}>
                <span class="ml-1 tabular-nums text-muted-foreground/70">{props.archivedCount}</span>
              </Show>
            </button>
          )}
        </For>
      </div>
    </div>
  );
}
