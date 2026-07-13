import { ChevronDown, Inbox as InboxIcon } from "lucide-solid";
import { onCleanup, onMount, Show } from "solid-js";
import { snippet } from "../../lib/format";
import {
  hasOpenRequiresResponse,
  isPingResolved,
  mostActionablePing,
  openUnreadCount,
} from "../../store/selectors";
import { setTrayExpanded, state } from "../../store/store";
import { PaneHeader } from "../ui/PaneHeader";
import { PingsView } from "./PingsView";

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

const PREVIEW_MAX = 52;

// All the shelf/rail counts fold in the optimistic Mark-done override
// (`resolveOverrides`), so a just-Marked-done Ping leaves the open counts / the
// One-Escalation red the instant it flips — not only once the 5s Undo window
// commits (spec item 2).
function openCount(): number {
  return state.pings.filter(
    (p) => p.status === "open" && !state.resolveOverrides.includes(p.id),
  ).length;
}

function doneCount(): number {
  return state.pings.filter((p) => isPingResolved(p, state.resolveOverrides)).length;
}

function badgeCount(): number {
  return openUnreadCount(state.pings, state.unreadOverrides, state.resolveOverrides);
}

function badgeLabel(): string {
  const n = badgeCount();
  return n > 99 ? "99+" : String(n);
}

function danger(): boolean {
  return hasOpenRequiresResponse(state.pings, state.resolveOverrides);
}

/** The most-actionable open Ping the collapsed shelf previews, or null. */
function previewPing() {
  return mostActionablePing(state.pings, state.unreadOverrides, state.resolveOverrides);
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
        class="flex h-10 w-full shrink-0 items-center gap-2 border-t border-border bg-card px-3 text-left rail:hidden"
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
              data-slot="tray-shelf-badge"
              class="grid h-4 min-w-4 shrink-0 place-items-center rounded-full px-1 text-[0.65rem] font-bold text-primary-foreground"
              classList={{ "bg-status-danger": danger(), "bg-muted-foreground": !danger() }}
            >
              {badgeLabel()}
            </span>
          </Show>
          <Show
            when={expanded()}
            fallback={
              <Show
                when={previewPing()}
                fallback={
                  <span class="min-w-0 flex-1 truncate text-xs text-muted-foreground">Pings</span>
                }
              >
                {(p) => (
                  <span class="flex min-w-0 flex-1 items-baseline gap-1.5 truncate">
                    <span class="shrink-0 font-mono text-xs text-foreground">@{p().name}</span>
                    <span class="min-w-0 truncate text-xs text-muted-foreground">
                      {snippet(p().description || p().content, PREVIEW_MAX)}
                    </span>
                  </span>
                )}
              </Show>
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

/** The standing Pings rail's header count. Same count selector as the phone
 * shelf, so the number holds parity — and now the desktop home of the ONE
 * sanctioned interrupt red (v2.3): with the nav "Inbox" badge gone, the rail
 * header carries the `status-danger` accent when an open requires-response Ping
 * exists (muted otherwise), so there is exactly one red source per surface —
 * the rail header on desktop, the shelf badge on phone. */
function PingsBadge(props: { slot?: string }) {
  return (
    <Show when={badgeCount() > 0}>
      <span
        data-slot={props.slot}
        class="grid h-4 min-w-4 shrink-0 place-items-center rounded-full px-1 text-[0.65rem] font-bold text-primary-foreground"
        classList={{ "bg-status-danger": danger(), "bg-muted-foreground": !danger() }}
      >
        {badgeLabel()}
      </span>
    </Show>
  );
}

/** The standing Pings rail (desktop-shell): the Tray promoted from a bottom
 * shelf to a persistent right column at `rail` width. Same `PingsView` body the
 * phone overlay uses (one component, two mount points), under a slim header that
 * mirrors the shelf's count/danger badge. It is the idle resting state of the
 * exclusive right region (v2.3) — rendered only while `rightRegion === "pings"`,
 * so opening any other pane UNMOUNTS it (nothing to clip, no hidden focus) — and
 * `hidden` below the rail breakpoint so the phone shelf/overlay stay the sole
 * Pings surface there. */
export function PingsRail() {
  return (
    <Show when={state.rightRegion === "pings"}>
      <aside
        data-slot="pings-rail"
        class="hidden min-h-0 w-[clamp(340px,38vw,440px)] shrink-0 flex-col border-l border-border bg-background rail:flex
          motion-safe:animate-in motion-safe:fade-in motion-safe:slide-in-from-right-2 motion-safe:duration-150"
        aria-label="Pings"
      >
        {/* Shared PaneHeader (spec item 1) so the right slot reads as ONE slot —
            same h-12 datum, icon rhythm, and title token as Canvas / Processes /
            Settings; the resting Pings home shows no × (it is the default, not a
            dismissible inspector) and carries its count badge instead. No
            agent-activity pulse here: the center chat header owns the single
            "what is my agent doing" indicator (One-Escalation Rule). */}
        <PaneHeader
          icon={<InboxIcon class="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />}
          title="Pings"
          badge={<PingsBadge slot="pings-rail-badge" />}
        />
        <PingsView />
      </aside>
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
      class="absolute inset-0 z-20 bg-background/65 backdrop-blur-[2px] motion-safe:animate-in motion-safe:fade-in motion-safe:duration-150 rail:hidden"
      aria-hidden="true"
      onClick={() => setTrayExpanded(false)}
    />
  );
}

/** The expanded panel: reuses `PingsView` (Done section included) verbatim
 * inside an absolutely-positioned box, ~58% viewport height, its own internal
 * scroll. `TrayShelf` (a flex sibling just below this container) supplies the
 * visual header/footer cap and the collapse control. */
function TrayPanel() {
  return (
    <div
      data-slot="tray-panel"
      class="absolute inset-x-0 bottom-0 z-30 flex h-[58dvh] max-h-full flex-col overflow-hidden rounded-t-xl border border-b-0 border-border bg-background shadow-lg rail:hidden
        motion-safe:animate-in motion-safe:slide-in-from-bottom motion-safe:duration-200"
      role="region"
      aria-label="Pings"
    >
      <PingsView />
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
