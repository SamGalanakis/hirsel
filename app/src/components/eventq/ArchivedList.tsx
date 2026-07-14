// The quiet Archived(n) view (archive contract v1): archived events as DENSE
// rows — @name · one-line title · a plain Unarchive action — never full cards.
// Archive is storage, not a second queue: the rows are deliberately flat and
// muted (no accent stripe, no options, no red), and Unarchive is the one verb.
// Shared verbatim by the desktop Feed column's archived section and the phone
// peek's archived section so the two surfaces never drift.
import { For, Show } from "solid-js";
import type { EventItem } from "../../protocol";
import { eventTitle } from "../../store/selectors";

export function ArchivedList(props: {
  events: EventItem[];
  onUnarchive: (ev: EventItem) => void;
}) {
  return (
    <div data-slot="archived-list" class="flex flex-col">
      <Show
        when={props.events.length > 0}
        fallback={
          <div class="px-2 py-2 text-xs text-muted-foreground/70">Nothing archived.</div>
        }
      >
        <For each={props.events}>
          {(ev) => (
            <div class="grid grid-cols-[1fr_auto] items-center gap-2.5 rounded-md px-2 py-1.5 transition-colors hover:bg-muted/40">
              <span class="min-w-0">
                <span class="block truncate font-mono text-[0.68rem] font-medium text-muted-foreground">
                  {ev.name}
                </span>
                <span class="block truncate text-xs text-muted-foreground">{eventTitle(ev)}</span>
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
      </Show>
    </div>
  );
}
