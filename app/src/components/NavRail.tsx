import { Activity, Command, Inbox as InboxIcon, MessagesSquare, Settings } from "lucide-solid";
import { type JSX, Show } from "solid-js";
import { setCommandPaletteOpen } from "../lib/keymap";
import {
  hasOpenRequiresResponse,
  openUnreadCount,
  runningProcessCount,
} from "../store/selectors";
import {
  setActiveSideChatSc,
  setProcessesOpen,
  setSettingsOpen,
  state,
} from "../store/store";
import { ConnectionPill } from "./ConnectionPill";

// The desktop nav rail (desktop-shell / 3-pane). A persistent vertical rail —
// calm and dense, Linear/Slack/Superhuman lineage — that owns the brand,
// primary navigation, and the connection pill on desktop. Desktop-only:
// `hidden … rail:flex`, so below the `rail` breakpoint the phone header carries
// the brand + Processes + connection instead and this rail never shows. Flat by
// design: a single hairline `border-r`, no shadow (the Hairline-First rule).

/** Shared count/danger for the Inbox item — the SAME selectors the Tray shelf
 * and Pings rail badges use, so count + danger tone hold parity by construction. */
function inboxCount(): number {
  return openUnreadCount(state.pings, state.unreadOverrides);
}
function inboxDanger(): boolean {
  return hasOpenRequiresResponse(state.pings);
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

      <nav aria-label="Primary" class="flex flex-col gap-0.5 px-2">
        {/* Chat — the calm default (chat + context). Returns to it without
            closing an open Side Chat. */}
        <NavItem
          icon={<MessagesSquare class="size-4 shrink-0" aria-hidden="true" />}
          label="Chat"
          active={!state.processesOpen && !state.settingsOpen}
          onClick={() => {
            setProcessesOpen(false);
            setSettingsOpen(false);
          }}
        />

        {/* Inbox — the desktop replacement for the old header "Show Pings"
            restore affordance. The unread badge (danger tone when a
            requires-response Ping is open) is its signal; no persistent active
            fill. Reveals the Pings rail by clearing the side chat. */}
        <NavItem
          icon={<InboxIcon class="size-4 shrink-0" aria-hidden="true" />}
          label="Inbox"
          ariaLabel="Inbox"
          onClick={() => {
            setProcessesOpen(false);
            setSettingsOpen(false);
            setActiveSideChatSc(null);
          }}
          badge={
            <Show when={inboxCount() > 0}>
              <span
                data-slot="nav-inbox-badge"
                class="ml-auto grid h-4 min-w-4 shrink-0 place-items-center rounded-full px-1 text-[0.65rem] font-bold text-primary-foreground"
                classList={{
                  "bg-status-danger": inboxDanger(),
                  "bg-muted-foreground": !inboxDanger(),
                }}
              >
                {clamp99(inboxCount())}
              </span>
            </Show>
          }
        />

        {/* Processes — toggles the Processes inspector; carries the running
            count in status-active tone (parity with the old header button). */}
        <NavItem
          icon={<Activity class="size-4 shrink-0" aria-hidden="true" />}
          label="Processes"
          active={state.processesOpen}
          onClick={() => {
            setSettingsOpen(false);
            setProcessesOpen(!state.processesOpen);
          }}
          badge={
            <Show when={processCount() > 0}>
              {/* Tint-chip vocabulary (not a solid saturated disc): a pulsing
                  status-active dot + a muted-strength count on a 15% tint. Only
                  the requires-response Inbox badge is ever loud/solid. */}
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

        {/* Settings — toggles the Settings inspector (also reachable from the
            phone header gear). */}
        <NavItem
          icon={<Settings class="size-4 shrink-0" aria-hidden="true" />}
          label="Settings"
          ariaLabel="Settings"
          active={state.settingsOpen}
          onClick={() => {
            setProcessesOpen(false);
            setSettingsOpen(!state.settingsOpen);
          }}
        />
      </nav>

      {/* Command palette affordance — summons the ⌘K surface (also bound
          globally in the keymap). Kept quiet: a ghost row with a mono keyhint,
          not standing chrome. The keyhint is the only mono in the rail. */}
      <div class="mt-1 px-2">
        <NavItem
          icon={<Command class="size-4 shrink-0" aria-hidden="true" />}
          label="Commands"
          ariaLabel="Open command palette"
          onClick={() => setCommandPaletteOpen(true)}
          badge={
            <kbd class="ml-auto grid h-5 shrink-0 place-items-center rounded-sm border border-border bg-muted px-1.5 font-mono text-[0.68rem] text-muted-foreground">
              ⌘K
            </kbd>
          }
        />
      </div>

      {/* Footer — the connection pill pinned to the foot, left-aligned and calm.
          (It also still renders in the phone header below the rail breakpoint.) */}
      <div class="mt-auto border-t border-border px-3 py-3">
        <ConnectionPill />
      </div>
    </div>
  );
}
