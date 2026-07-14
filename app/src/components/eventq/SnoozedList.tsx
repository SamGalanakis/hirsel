// The quiet "Snoozed (n)" view (Wave-3 durable snooze): parked events as DENSE
// rows — @name · one-line title · the return time · a plain Unsnooze action —
// never full cards. The snooze parking lot is storage, not a second queue: the
// rows are deliberately flat and muted (no accent stripe, no options, nothing
// red), and Unsnooze is the one verb. Shared verbatim by the desktop Feed
// column's snoozed section and the phone peek's, so the two never drift.
import { For, Show } from "solid-js";
import { Clock } from "lucide-solid";
import type { EventItem } from "../../protocol";
import { formatReturnTime } from "../../lib/format";
import { eventTitle } from "../../store/selectors";

export function SnoozedList(props: {
  events: EventItem[];
  onUnsnooze: (ev: EventItem) => void;
}) {
  return (
    <div data-slot="snoozed-list" class="flex flex-col">
      <Show
        when={props.events.length > 0}
        fallback={
          <div class="px-2 py-2 text-xs text-muted-foreground/70">Nothing snoozed.</div>
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
              <span class="flex shrink-0 items-center gap-2.5">
                <Show when={ev.snoozed_until}>
                  <span class="inline-flex items-center gap-1 text-[0.68rem] tabular-nums text-muted-foreground/80">
                    <Clock class="size-3" aria-hidden="true" />
                    {formatReturnTime(ev.snoozed_until!)}
                  </span>
                </Show>
                <button
                  type="button"
                  aria-label={`Unsnooze ${ev.name}: ${eventTitle(ev)}`}
                  class="rounded-sm text-[0.68rem] font-semibold text-primary transition-colors hover:text-primary/80 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
                  onClick={() => props.onUnsnooze(ev)}
                >
                  Unsnooze
                </button>
              </span>
            </div>
          )}
        </For>
      </Show>
    </div>
  );
}
