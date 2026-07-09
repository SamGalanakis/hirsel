import { ChevronDown, Inbox as InboxIcon } from "lucide-solid";
import { onCleanup, onMount, Show } from "solid-js";
import { snippet } from "../../lib/format";
import {
  hasOpenRequiresResponse,
  isResolvedStatus,
  mostActionableItem,
  openUnreadCount,
} from "../../store/selectors";
import { setTrayExpanded, state } from "../../store/store";
import { InboxView } from "./InboxView";

// Tray (ADR-0008 / design critique [P1]): the Inbox tab is gone. Collapsed, it
// is a slim shelf pinned directly above the Composer; expanded, it is an
// overlay over the message area that never pushes the chat. `TrayShelf` is a
// single persistent control strip that plays both roles — its content swaps
// between "collapsed preview" and "expanded header" — so "collapse via the
// shelf header" is literally the same element, not two different affordances.
// `TrayOverlay` renders the absolutely-positioned scrollable panel; it must be
// mounted inside ChatView's `relative` message-area container so it overlays
// (rather than reflows) the scroller, while `TrayShelf` is a normal flex
// sibling between that container and the Composer.

const PREVIEW_MAX = 60;

function openCount(): number {
  return state.inbox.filter((i) => i.status === "open").length;
}

function doneCount(): number {
  return state.inbox.filter((i) => isResolvedStatus(i.status)).length;
}

function badgeCount(): number {
  return openUnreadCount(state.inbox, state.unreadOverrides);
}

function badgeLabel(): string {
  const n = badgeCount();
  return n > 99 ? "99+" : String(n);
}

function danger(): boolean {
  return hasOpenRequiresResponse(state.inbox);
}

function preview(): string | null {
  const item = mostActionableItem(state.inbox, state.unreadOverrides);
  return item ? snippet(item.content, PREVIEW_MAX) : null;
}

const thinking = () => state.agentActivity.state === "thinking";

/** Collapsed shelf / expanded header, ~40px, pinned directly above the
 * Composer. Hidden entirely when there are no open items; if only Deleted
 * items remain, a minimal handle keeps them reachable (no standing
 * empty-inbox chrome). Tap toggles expand/collapse — never auto-expanded. */
export function TrayShelf() {
  const hasOpen = () => openCount() > 0;
  const hasDone = () => doneCount() > 0;
  const expanded = () => state.trayExpanded;

  return (
    <Show when={hasOpen() || hasDone()}>
      <button
        type="button"
        data-slot="tray-bar"
        class="flex h-10 w-full shrink-0 items-center gap-2 border-t border-border bg-card px-3 text-left"
        onClick={() => setTrayExpanded(!expanded())}
        aria-expanded={expanded()}
        aria-label={
          expanded() ? "Collapse Pings" : hasOpen() ? "Open Pings" : "Open done items"
        }
      >
        <Show
          when={hasOpen()}
          fallback={<span class="text-xs text-muted-foreground">Done ({doneCount()})</span>}
        >
          <InboxIcon class="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
          <Show when={badgeCount() > 0}>
            <span
              class="grid h-4 min-w-4 shrink-0 place-items-center rounded-full px-1 text-[0.65rem] font-bold text-primary-foreground"
              classList={{ "bg-status-danger": danger(), "bg-muted-foreground": !danger() }}
            >
              {badgeLabel()}
            </span>
          </Show>
          <Show
            when={expanded()}
            fallback={
              <span class="min-w-0 flex-1 truncate text-xs text-muted-foreground">
                {preview()}
              </span>
            }
          >
            <span class="min-w-0 flex-1 truncate text-xs font-medium text-foreground">Pings</span>
          </Show>
          <Show when={expanded() && thinking()}>
            <span class="flex shrink-0 items-center gap-1.5 text-[0.68rem] text-muted-foreground">
              <span
                class="size-1.5 animate-pulse rounded-full bg-status-active"
                aria-hidden="true"
              />
              agent working
            </span>
          </Show>
          <ChevronDown
            class="size-4 shrink-0 text-muted-foreground transition-transform"
            classList={{ "rotate-180": !expanded() }}
            aria-hidden="true"
          />
        </Show>
      </button>
    </Show>
  );
}

/** Transparent scrim confined to the message-area container (never extends
 * over the Composer, which must stay usable while the tray is up). Tapping it
 * — "tapping outside" — collapses the tray; Esc does too. Its own
 * mount/cleanup lifecycle (via the parent `<Show>`) is what scopes the global
 * Esc listener to exactly when the overlay is open. */
function TrayScrim() {
  onMount(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") setTrayExpanded(false);
    };
    window.addEventListener("keydown", onKeyDown);
    onCleanup(() => window.removeEventListener("keydown", onKeyDown));
  });

  return (
    <div
      class="absolute inset-0 z-20"
      aria-hidden="true"
      onClick={() => setTrayExpanded(false)}
    />
  );
}

/** The expanded panel: reuses `InboxView` (Done section included) verbatim
 * inside an absolutely-positioned box, ~58% viewport height, its own internal
 * scroll. `TrayShelf` (a flex sibling just below this container) supplies the
 * visual header/footer cap and the collapse control. */
function TrayPanel() {
  return (
    <div
      data-slot="tray-panel"
      class="absolute inset-x-0 bottom-0 z-30 flex h-[58dvh] max-h-full flex-col overflow-hidden rounded-t-xl border border-b-0 border-border bg-background shadow-lg"
      role="region"
      aria-label="Pings"
    >
      <InboxView />
    </div>
  );
}

/** Mount inside ChatView's `relative` message-area container so the panel
 * overlays the scroller instead of pushing it — the [P1] anti-pattern this
 * design exists to avoid. Renders nothing when collapsed. */
export function TrayOverlay() {
  return (
    <Show when={state.trayExpanded}>
      <TrayScrim />
      <TrayPanel />
    </Show>
  );
}
