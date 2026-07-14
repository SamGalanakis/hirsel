// The quiet Archived(n) view (archive contract v1) — the day-log (Wave-3 time
// axis): archived events as DENSE rows — @name · one-line title · a plain
// Unarchive action — never full cards, grouped under calm day headers ("Today",
// "Yesterday", then dates) ordered newest-first by when each was swept
// (`archived_at`). Archive is storage, not a second queue: the rows are
// deliberately flat and muted (no accent stripe, no options, no red), and
// Unarchive is the one verb. Shared verbatim by the desktop Feed column's
// archived section and the phone peek's, so the two never drift.
import { createMemo, For, Show } from "solid-js";
import type { EventItem } from "../../protocol";
import { formatDayGroup } from "../../lib/format";
import { eventTitle } from "../../store/selectors";

/** Group the (already newest-first) archived events into consecutive day runs,
 * keyed by the day they were swept. A missing `archived_at` (an optimistic row
 * before the host echo) falls into "Today" — a fresh sweep is today's work. */
function dayGroups(events: EventItem[]): { label: string; events: EventItem[] }[] {
  const groups: { label: string; events: EventItem[] }[] = [];
  for (const ev of events) {
    const label = ev.archived_at ? formatDayGroup(ev.archived_at) : "Today";
    const last = groups[groups.length - 1];
    if (last && last.label === label) last.events.push(ev);
    else groups.push({ label, events: [ev] });
  }
  return groups;
}

export function ArchivedList(props: {
  events: EventItem[];
  onUnarchive: (ev: EventItem) => void;
}) {
  const groups = createMemo(() => dayGroups(props.events));
  return (
    <div data-slot="archived-list" class="flex flex-col">
      <Show
        when={props.events.length > 0}
        fallback={
          <div class="px-2 py-2 text-xs text-muted-foreground/70">Nothing archived.</div>
        }
      >
        <For each={groups()}>
          {(group) => (
            <div class="flex flex-col">
              <div class="px-2 pb-0.5 pt-2 text-[0.62rem] font-semibold uppercase tracking-[0.04em] text-muted-foreground/70">
                {group.label}
              </div>
              <For each={group.events}>
                {(ev) => (
                  <div class="grid grid-cols-[1fr_auto] items-center gap-2.5 rounded-md px-2 py-1.5 transition-colors hover:bg-muted/40">
                    <span class="min-w-0">
                      <span class="block truncate font-mono text-[0.68rem] font-medium text-muted-foreground">
                        {ev.name}
                      </span>
                      <span class="block truncate text-xs text-muted-foreground">
                        {eventTitle(ev)}
                      </span>
                    </span>
                    <button
                      type="button"
                      aria-label={`Unarchive ${ev.name}: ${eventTitle(ev)}`}
                      class="shrink-0 rounded-sm text-[0.68rem] font-semibold text-primary transition-colors hover:text-primary/80 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
                      onClick={() => props.onUnarchive(ev)}
                    >
                      Unarchive
                    </button>
                  </div>
                )}
              </For>
            </div>
          )}
        </For>
      </Show>
    </div>
  );
}
