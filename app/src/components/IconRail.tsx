import { Activity, Settings } from "lucide-solid";
import { type JSX, Show } from "solid-js";
import { setCommandPaletteOpen } from "../lib/keymap";
import { runningProcessCount } from "../store/selectors";
import { openProcesses, openSettings, state } from "../store/store";
import { BrandMark } from "./BrandMark";
import { ConnectionPill } from "./ConnectionPill";

// The desktop icon rail (desktop-unified shell). The pre-unified desktop made
// Queue and Chat destinations you SWITCHED between via a 224px nav rail — which,
// beside the queue's own index column, was the "two sidebars" the owner called
// out. The unified shell shows Feed AND Chat at once, so those destination rows
// are meaningless and gone; what is left of the left rail is just global chrome:
// the brand, the observable-fleet launcher (Processes), and a footer strip
// (connection · Settings · ⌘K). That is far too little to justify a wide column
// beside a Feed column beside Chat, so the rail collapses to a slim ~56px
// ICON strip — the Slack/Superhuman lineage, calm and out of the way, reclaiming
// the width for the two content panes. Desktop-only (`hidden … rail:flex`): below
// the rail the phone header carries brand + agent status + overflow instead.
//
// Nothing here is a "destination" (Feed and Chat are always visible); Processes
// and Settings DOCK the shared right region and read `aria-current` exactly while
// they own it (muted fill, never indigo — the One-Escalation / indigo-is-rare
// rules hold). The rail carries no count-bearing red: the surface's one red is
// the Feed column's needs-you pill.

function processCount(): number {
  return runningProcessCount(state.processes);
}
function clamp99(n: number): string {
  return n > 99 ? "99+" : String(n);
}

/** One square icon affordance, centred in the slim rail. Active (owns the right
 * region) → muted fill + full-strength foreground, `aria-current="page"`. */
function RailButton(props: {
  icon: JSX.Element;
  label: string;
  active?: boolean;
  onClick: () => void;
  badge?: JSX.Element;
}) {
  return (
    <button
      type="button"
      class="relative flex size-10 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
      classList={{ "bg-muted text-foreground": props.active }}
      aria-current={props.active ? "page" : undefined}
      aria-label={props.label}
      title={props.label}
      onClick={props.onClick}
    >
      {props.icon}
      {props.badge}
    </button>
  );
}

export function IconRail() {
  return (
    <div class="hidden w-14 shrink-0 flex-col items-center border-r border-border bg-background rail:flex">
      {/* Brand block — the wordmark folded to a monogram tile on the shared h-12
          top datum, so one continuous top hairline runs across the rail, the Feed
          header, and the chat header. No destination to navigate to (Feed + Chat
          both stand), so it is identity, not a control. */}
      <div
        class="flex h-12 w-full flex-shrink-0 items-center justify-center border-b border-border"
        role="img"
        aria-label="hirsel"
        title="hirsel"
      >
        <BrandMark size={24} />
      </div>

      <nav aria-label="Primary" class="flex flex-col items-center gap-1 pt-2">
        {/* Processes — docks the observable-fleet inspector. Running work shows as
            a status-active tint DOT (never the interrupt red, never a solid disc);
            the count is exposed to AT via the label. */}
        <RailButton
          icon={<Activity class="size-5 shrink-0" aria-hidden="true" />}
          label={processCount() > 0 ? `Processes, ${clamp99(processCount())} running` : "Processes"}
          active={state.rightRegion === "processes"}
          onClick={openProcesses}
          badge={
            <Show when={processCount() > 0}>
              <span
                data-slot="rail-processes-badge"
                class="absolute right-1.5 top-1.5 size-1.5 rounded-full bg-status-active motion-safe:animate-pulse"
                aria-hidden="true"
              />
            </Show>
          }
        />
      </nav>

      {/* Footer strip — the calm global chrome, stacked at the foot: the
          connection dot, the Settings gear (docks the Settings inspector, muted
          active fill while it owns the region), and the ⌘K command-palette keycap.
          Settings is chrome you reach for, not a destination, so it lives here
          rather than in the Primary nav. */}
      <div class="mt-auto flex flex-col items-center gap-1.5 pb-3">
        <ConnectionPill compact />
        <RailButton
          icon={<Settings class="size-5 shrink-0" aria-hidden="true" />}
          label="Settings"
          active={state.rightRegion === "settings"}
          onClick={() => openSettings()}
        />
        <button
          type="button"
          class="flex items-center justify-center rounded-sm text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
          aria-label="⌘K — Open command palette"
          title="Command palette"
          onClick={() => setCommandPaletteOpen(true)}
        >
          <kbd class="grid h-5 place-items-center rounded-sm border border-border bg-muted px-1 font-mono text-[0.62rem] text-muted-foreground">
            ⌘K
          </kbd>
        </button>
      </div>
    </div>
  );
}
