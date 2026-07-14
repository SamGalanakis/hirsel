// Shared event-card pieces — the parts of a queue card that must look identical
// wherever a card is shown: the phone scroller's full-viewport pager (EventPage)
// and the desktop Feed column (FeedCard). Only the COMPOSITION around the card
// differs (a swipe wrapper + flick affordances on phone, a static scroll item on
// desktop); the card's own chrome — the needs-you accent, the minimal header
// (source, read, archive — the question leads from the body, not the header),
// and the decided/undo strip — lives here so the two never drift.
//
// The card BODY is the constrained JSON UI (`EventCardRenderer`) and the decide
// path is `lib/event-decide` — both already shared. This module adds the last
// shared visual atoms so "desktop is a composition change, not a re-render of
// cards" holds literally.
import { ArrowRight, Check, MoreHorizontal } from "lucide-solid";
import { createSignal, Show } from "solid-js";
import type { EventItem } from "../../protocol";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { isEventFinished, isEventResolved } from "../../store/selectors";
import { state } from "../../store/store";

/** How long the archive exit animation runs before the card actually leaves the
 * set (the optimistic sweep dispatch). Short and calm — a settle, not a flourish. */
export const ARCHIVE_EXIT_MS = 200;

/** Shared motion-safe archive exit for both card compositions: `archive()` runs
 * the brief fade/settle (skipped under prefers-reduced-motion) and then fires
 * `commit` (the real archive), so the card animates out before the re-flow.
 * `leaving()` drives the exit classes on the card box. */
export function createArchiveExit(commit: () => void): {
  leaving: () => boolean;
  archive: () => void;
} {
  const [leaving, setLeaving] = createSignal(false);
  const archive = () => {
    if (leaving()) return;
    const reduced =
      typeof window !== "undefined" &&
      window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    if (reduced) {
      commit();
      return;
    }
    setLeaving(true);
    setTimeout(commit, ARCHIVE_EXIT_MS);
  };
  return { leaving, archive };
}

/** The exit classes `leaving()` applies to the card box — one string so the two
 * compositions animate identically. */
export const ARCHIVE_EXIT_CLASS =
  "pointer-events-none scale-[0.98] opacity-0 transition-[opacity,transform] duration-200 ease-out motion-reduce:transition-none";

/** The card's top matter: the one-accent needs-you edge (a hairline indigo strip
 * — the surface's single accent, never red) plus a MINIMAL header row. The mono
 * `@name` slug has left the card FACE (it still lives in search, the archive
 * aria-label, @-mentions, the DecidedStrip and the archived rows) and the kind
 * micro-label is gone — the card SHAPE says the kind, and the heading (the
 * question, drawn by the body below) leads. The header now carries only the
 * source (when it isn't the resident agent), the read tag, and the archive
 * overflow. Placed as the first children of a `relative overflow-hidden` card box.
 *
 * A FINISHED event (decided, or read awareness — the exact `events.clear` set)
 * also carries the quiet ⋯ overflow with the Archive action: reversible (the
 * Archived view's Unarchive), so no confirmation and nothing red. An open
 * judgment never offers it — deciding is how a judgment leaves the queue. */
export function EventCardHeader(props: { ev: EventItem; onArchive?: (ev: EventItem) => void }) {
  const decided = () => isEventResolved(props.ev, state.eventDecideOverrides);
  const isJudgment = () => props.ev.kind === "judgment";
  const archivable = () =>
    props.onArchive !== undefined && isEventFinished(props.ev, state.eventDecideOverrides);
  // Source earns a line only when it isn't the resident agent — a subagent,
  // scheduled job, or monitor is worth naming; "agent" is the default voice and
  // saying so is noise.
  const showSource = () => props.ev.source.kind !== "agent";
  return (
    <>
      <Show when={isJudgment() && !decided()}>
        <span class="absolute inset-y-0 left-0 w-0.5 bg-primary" aria-hidden="true" />
      </Show>
      <div class="flex flex-wrap items-center gap-x-2 gap-y-1 px-3.5 pt-3">
        <Show when={showSource()}>
          <span class="text-[0.68rem] font-semibold text-muted-foreground">
            {props.ev.source.ref ?? props.ev.source.kind}
          </span>
        </Show>
        <span class="flex-1" />
        <Show when={props.ev.read && props.ev.kind !== "judgment" && !decided()}>
          <span class="inline-flex items-center gap-1 text-[0.62rem] font-bold uppercase tracking-[0.04em] text-status-success">
            <Check class="size-3" aria-hidden="true" /> read
          </span>
        </Show>
        <Show when={archivable()}>
          <DropdownMenu>
            <DropdownMenuTrigger
              class="-my-1 grid size-6 place-items-center rounded-md text-muted-foreground/70 transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
              aria-label={`More actions for ${props.ev.name}`}
            >
              <MoreHorizontal class="size-4" aria-hidden="true" />
            </DropdownMenuTrigger>
            <DropdownMenuContent>
              <DropdownMenuItem onSelect={() => props.onArchive?.(props.ev)}>
                Archive
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </Show>
      </div>
    </>
  );
}

/** The interaction-back confirmation that replaces the card's action row once
 * decided: a green check + the posted payload + a brief Undo — and Archive, the
 * natural "done with this" exit now that the decision is posted. Shared verbatim
 * by the phone pager and the desktop Feed column. */
export function DecidedStrip(props: {
  ev: EventItem;
  onUndo: (id: number) => void;
  onArchive?: (ev: EventItem) => void;
}) {
  return (
    <div class="border-t border-border bg-muted/40 px-3.5 py-3">
      <div class="flex items-center gap-2">
        <span class="grid size-[18px] shrink-0 place-items-center rounded-full bg-status-success/15 text-status-success">
          <Check class="size-3" aria-hidden="true" />
        </span>
        <span class="text-xs font-semibold text-foreground">
          {props.ev.name} → <span class="text-status-success">decided</span>
        </span>
      </div>
      <div class="mt-2 flex items-center gap-3 text-[0.68rem] text-muted-foreground">
        <span class="inline-flex min-w-0 items-center gap-2">
          <ArrowRight class="size-3 shrink-0 text-primary" aria-hidden="true" />
          <span class="truncate">posted to {props.ev.source.ref ?? props.ev.source.kind}</span>
        </span>
        <span class="flex-1" />
        <Show when={props.onArchive}>
          <button
            type="button"
            class="font-semibold text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
            onClick={() => props.onArchive?.(props.ev)}
          >
            Archive
          </button>
        </Show>
        <button
          type="button"
          class="font-bold text-primary transition-colors hover:text-primary/80"
          onClick={() => props.onUndo(props.ev.id)}
        >
          Undo
        </button>
      </div>
    </div>
  );
}
