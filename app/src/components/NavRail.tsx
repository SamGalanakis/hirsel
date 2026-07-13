import { Activity, Inbox as InboxIcon, Settings } from "lucide-solid";
import { type JSX, Show } from "solid-js";
import { setCommandPaletteOpen } from "../lib/keymap";
import { openUnreadCount, runningProcessCount } from "../store/selectors";
import { openPings, openProcesses, openSettings, state } from "../store/store";
import { ConnectionPill } from "./ConnectionPill";

// The desktop nav rail (desktop-shell / 3-pane). A persistent vertical rail —
// calm and dense, Linear/Slack/Superhuman lineage — that owns the brand, the
// right-region navigation, and the connection pill on desktop. Desktop-only:
// `hidden … rail:flex`, so below the `rail` breakpoint the phone header carries
// the brand + agent status + overflow instead and this rail never shows. Flat by
// design: a single hairline `border-r`, no shadow (the Hairline-First rule).
//
// v2.3 (single-owner right region): the rail navigates the ONE exclusive right
// region — Pings · Processes · Settings — and each item is `aria-current` when
// it owns `state.rightRegion` (no more "Chat active = !processesOpen &&
// !settingsOpen" fiction). Chat is always the center pane, so it is no longer a
// nav destination; the standing right rail IS the Pings surface, so there is no
// separate "Inbox" row; the command palette is ⌘K (a quiet footer hint, not a
// standing "Commands" row).

function pingsCount(): number {
  return openUnreadCount(state.pings, state.unreadOverrides, state.resolveOverrides);
}
function processCount(): number {
  return runningProcessCount(state.processes);
}
function clamp99(n: number): string {
  return n > 99 ? "99+" : String(n);
}

/** One rail row. Active items get the muted fill + full-strength foreground
 * (NOT indigo — per DESIGN, indigo stays reserved for "attend to this"), and
 * `aria-current="page"`. An optional right-aligned badge rides in `ml-auto`. */
function NavItem(props: {
  icon: JSX.Element;
  label: string;
  active?: boolean;
  ariaLabel?: string;
  onClick: () => void;
  badge?: JSX.Element;
}) {
  return (
    <button
      type="button"
      class="flex w-full items-center gap-2.5 rounded-md px-2.5 py-2 text-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
      classList={{ "bg-muted text-foreground": props.active }}
      aria-current={props.active ? "page" : undefined}
      aria-label={props.ariaLabel}
      onClick={props.onClick}
    >
      {props.icon}
      <span class="min-w-0 flex-1 truncate text-left">{props.label}</span>
      {props.badge}
    </button>
  );
}

export function NavRail() {
  return (
    <div class="hidden w-[224px] shrink-0 flex-col border-r border-border bg-background rail:flex">
      {/* Brand block — the wordmark on the shared desktop top-bar baseline (h-12,
          same as the center chat header and the Pings/inspector pane headers), so
          one continuous top hairline runs across all three panes. Kept quiet
          (reuses the title type). */}
      <div class="flex h-12 flex-shrink-0 items-center border-b border-border px-4">
        <h1 class="m-0 text-base font-semibold tracking-[0.01em]">hirsel</h1>
      </div>

      <nav aria-label="Primary" class="flex flex-col gap-0.5 px-2 pt-2">
        {/* Pings — the resting state of the right region (the standing rail).
            Active when it owns the region. A MUTED count rides here for
            cross-pane awareness ONLY when Pings is NOT the shown pane: when
            `rightRegion === "pings"` (the default), the standing rail header
            already carries the count, so the nav badge would just duplicate it
            (spec item 4) — the nav ITEM stays (the way home + `g i` target + the
            count's home when the rail is displaced), only the redundant badge
            is hidden. The single red interrupt lives on the rail header
            (One-Escalation Rule), never on the nav. */}
        <NavItem
          icon={<InboxIcon class="size-4 shrink-0" aria-hidden="true" />}
          label="Pings"
          ariaLabel="Pings"
          active={state.rightRegion === "pings"}
          onClick={openPings}
          badge={
            <Show when={pingsCount() > 0 && state.rightRegion !== "pings"}>
              <span
                data-slot="nav-pings-badge"
                class="ml-auto grid h-4 min-w-4 shrink-0 place-items-center rounded-full bg-muted-foreground px-1 text-[0.65rem] font-bold text-primary-foreground"
              >
                {clamp99(pingsCount())}
              </span>
            </Show>
          }
        />

        {/* Processes — docks the Processes inspector; carries the running count
            in a status-active tint chip (parity with the phone overflow). */}
        <NavItem
          icon={<Activity class="size-4 shrink-0" aria-hidden="true" />}
          label="Processes"
          active={state.rightRegion === "processes"}
          onClick={openProcesses}
          badge={
            <Show when={processCount() > 0}>
              {/* Tint-chip vocabulary (not a solid saturated disc): a pulsing
                  status-active dot + a muted-strength count on a 15% tint. Only
                  the requires-response Pings badge is ever loud/solid. */}
              <span
                data-slot="nav-processes-badge"
                class="ml-auto flex h-4 shrink-0 items-center gap-1 rounded-full bg-status-active/15 px-1.5 text-[0.62rem] font-semibold text-status-active"
              >
                <span
                  class="size-1.5 shrink-0 rounded-full bg-status-active motion-safe:animate-pulse"
                  aria-hidden="true"
                />
                {clamp99(processCount())}
              </span>
            </Show>
          }
        />

        {/* Settings — docks the Settings inspector (also reachable from the phone
            header overflow). */}
        <NavItem
          icon={<Settings class="size-4 shrink-0" aria-hidden="true" />}
          label="Settings"
          ariaLabel="Settings"
          active={state.rightRegion === "settings"}
          onClick={openSettings}
        />
      </nav>

      {/* Footer — the connection pill pinned to the foot, with a quiet ⌘K
          command-palette hint beside it. The hint is a keycap affordance, not a
          standing nav row (the palette is summoned, never chrome). */}
      <div class="mt-auto flex items-center justify-between gap-2 border-t border-border px-3 py-3">
        <ConnectionPill />
        <button
          type="button"
          class="flex shrink-0 items-center rounded-sm text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          aria-label="Open command palette"
          onClick={() => setCommandPaletteOpen(true)}
        >
          <kbd class="grid h-5 place-items-center rounded-sm border border-border bg-muted px-1.5 font-mono text-[0.68rem] text-muted-foreground">
            ⌘K
          </kbd>
        </button>
      </div>
    </div>
  );
}
