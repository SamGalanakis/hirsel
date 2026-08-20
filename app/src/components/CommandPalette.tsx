// The ⌘K command palette + the `?` / ⌘/ shortcut cheat-sheet — two summoned,
// never standing surfaces. Both are calm Kobalte `Dialog`s (portal + scrim +
// their own focus trap + Esc-to-dismiss). Kobalte's trap is internal and never
// touches our trap stack, so each dialog joins `anyOverlayOpen` explicitly via
// `createOverlayPresence` on its open-state prop — without that the global
// bare-key layer keeps firing underneath the modal, and Esc double-fires. The
// palette is a filterable command list (a combobox/listbox pattern: a search
// field driving a `role="listbox"` with `aria-activedescendant`), styled to the
// calm-terminal register — Inter for labels, mono only for the keyboard hints.

import * as Dialog from "@kobalte/core/dialog";
import {
  Activity,
  Archive,
  ArrowDownToLine,
  CircleStop,
  Clock,
  Layers,
  MessagesSquare,
  Minimize2,
  Scale,
  Search,
  Settings,
  Trash2,
} from "lucide-solid";
import { type Component, createEffect, createMemo, createSignal, For, type JSX, Show } from "solid-js";
import type { EventItem } from "../protocol";
import { clearTaskFocus, state } from "../store/store";
import { archiveEventWithUndo } from "../lib/event-archive";
import { decideEventWithUndo } from "../lib/event-decide";
import { snoozeEventWithUndo } from "../lib/event-snooze";
import { clearFinishedEventsWithUndo } from "../lib/event-sweep";
import { snoozePresets } from "../lib/snooze-presets";
import { focusComposer, goPane, jumpToLatest, SHORTCUTS, stopActiveTurn } from "../lib/keymap";
import { createOverlayPresence } from "../lib/focus";
import {
  eventUiNodes,
  finishedEvents,
  isOpenJudgment,
  orderedTasks,
  visibleEvents,
} from "../store/selectors";
import { cn } from "@/lib/utils";

interface Command {
  id: string;
  label: string;
  /** Optional keyboard-hint tokens, rendered as mono chips on the right. */
  hint?: string[];
  keywords?: string;
  icon: JSX.Element;
  run: () => void;
}

/** Lightweight fuzzy match (no dep): case-insensitive subsequence — the query's
 * characters appear in order somewhere in the text (so "clrf" finds "Clear
 * finished"). Substring is the trivial subsequence case, so exact typing still
 * matches first. */
function fuzzyMatch(query: string, text: string): boolean {
  const q = query.trim().toLowerCase();
  if (q.length === 0) return true;
  const t = text.toLowerCase();
  let i = 0;
  for (let j = 0; j < t.length && i < q.length; j++) {
    if (t[j] === q[i]) i++;
  }
  return i === q.length;
}

/** The judgment the contextual actions ("Decide …", "Snooze current", "Archive
 * current") target: the first open judgment in Task priority
 * order (blocking first) — the one that most needs the Owner. */
function currentJudgment(): EventItem | null {
  const ordered = orderedTasks(
    visibleEvents(state.events, state.eventArchiveOverrides),
    state.eventDecideOverrides,
  );
  return ordered.find((e) => isOpenJudgment(e, state.eventDecideOverrides)) ?? null;
}

/** The letter-keyed options of a judgment's optionList, for the "Decide <key>"
 * entries. */
function judgmentOptions(ev: EventItem): { action: string; key: string; label: string }[] {
  const list = eventUiNodes(ev.ui).find((n) => n.type === "optionList");
  if (!list) return [];
  const action = typeof list.action === "string" ? list.action : "choose";
  const options = (Array.isArray(list.options) ? list.options : []) as Record<string, unknown>[];
  return options.map((o) => ({
    action,
    key: String(o.key ?? ""),
    label: String(o.label ?? "").replace(/`/g, ""),
  }));
}

// ---- Palette ----------------------------------------------------------------

export const CommandPalette: Component<{
  open: boolean;
  onOpenChange: (open: boolean) => void;
}> = (props) => {
  const [query, setQuery] = createSignal("");
  const [activeIndex, setActiveIndex] = createSignal(0);

  createOverlayPresence(() => props.open);

  const thinking = () => state.agentActivity.state === "thinking";
  const iconClass = "size-4 shrink-0 text-muted-foreground";

  // The full command set, rebuilt reactively so the current task actions and
  // stop-turn action track store state.
  const commands = createMemo<Command[]>(() => {
    const out: Command[] = [
      {
        id: "focus-composer",
        label: "Focus Hirsel",
        hint: ["/"],
        keywords: "type write message reply",
        icon: <MessagesSquare class={iconClass} aria-hidden="true" />,
        run: focusComposer,
      },
      {
        id: "go-tasks",
        label: "Focus tasks",
        hint: ["g", "t"],
        keywords: "tasks work judgments needs you",
        icon: <Layers class={iconClass} aria-hidden="true" />,
        run: () => goPane("tasks"),
      },
      {
        id: "go-processes",
        label: "Open Processes",
        hint: ["g", "p"],
        keywords: "monitors timers background",
        icon: <Activity class={iconClass} aria-hidden="true" />,
        run: () => goPane("processes"),
      },
      {
        id: "go-settings",
        label: "Open Settings",
        hint: ["g", "s"],
        keywords: "theme token endpoint",
        icon: <Settings class={iconClass} aria-hidden="true" />,
        run: () => goPane("settings"),
      },
      {
        id: "jump-latest",
        label: "Jump to latest message",
        hint: ["G"],
        keywords: "bottom newest end",
        icon: <ArrowDownToLine class={iconClass} aria-hidden="true" />,
        run: jumpToLatest,
      },
    ];

    // The exit from a focused Task, mirroring the Esc ladder's last rung. Only
    // offered while there is a focus to leave.
    if (state.focusedTaskId !== null) {
      out.push({
        id: "clear-focus",
        label: "Clear task focus",
        hint: ["Esc"],
        keywords: "ambient leave exit close unfocus back",
        icon: <Minimize2 class={iconClass} aria-hidden="true" />,
        run: clearTaskFocus,
      });
    }

    if (thinking()) {
      out.push({
        id: "stop-turn",
        label: "Stop the active turn",
        keywords: "cancel halt interrupt",
        icon: <CircleStop class={iconClass} aria-hidden="true" />,
        run: stopActiveTurn,
      });
    }

    // Contextual task actions stay flat so the palette remains fast.
    const ev = currentJudgment();
    if (ev) {
      for (const opt of judgmentOptions(ev)) {
        out.push({
          id: `decide-${ev.id}-${opt.key}`,
          label: `Decide ${opt.key} — ${opt.label}`,
          keywords: `choose answer ${ev.name} ${ev.description}`,
          icon: <Scale class={iconClass} aria-hidden="true" />,
          run: () => decideEventWithUndo(ev.id, opt.action, { choice: opt.key, label: opt.label }, opt.label),
        });
      }
      for (const preset of snoozePresets()) {
        out.push({
          id: `snooze-${ev.id}-${preset.key}`,
          label: `Snooze current · ${preset.label}`,
          keywords: `defer later ${ev.name}`,
          icon: <Clock class={iconClass} aria-hidden="true" />,
          run: () => snoozeEventWithUndo(ev.id, preset.until, preset.label),
        });
      }
      out.push({
        id: `archive-${ev.id}`,
        label: "Archive current",
        keywords: `dismiss ${ev.name}`,
        icon: <Archive class={iconClass} aria-hidden="true" />,
        run: () => archiveEventWithUndo(ev.id),
      });
    }

    const finishedIds = finishedEvents(
      state.events,
      state.eventArchiveOverrides,
      state.eventDecideOverrides,
    ).map((e) => e.id);
    if (finishedIds.length > 0) {
      out.push({
        id: "clear-finished",
        label: `Clear finished (${finishedIds.length})`,
        keywords: "sweep archive done",
        icon: <Trash2 class={iconClass} aria-hidden="true" />,
        run: () => clearFinishedEventsWithUndo(finishedIds),
      });
    }

    return out;
  });

  const filtered = createMemo<Command[]>(() => {
    const q = query().trim();
    if (!q) return commands();
    return commands().filter((c) => fuzzyMatch(q, `${c.label} ${c.keywords ?? ""}`));
  });

  // Reset the surface each time it is summoned, and keep the active row in range
  // as the filter narrows.
  createEffect(() => {
    if (props.open) {
      setQuery("");
      setActiveIndex(0);
    }
  });
  createEffect(() => {
    const n = filtered().length;
    if (activeIndex() >= n) setActiveIndex(n > 0 ? n - 1 : 0);
  });

  function runCommand(cmd: Command) {
    props.onOpenChange(false);
    // Defer past the dialog's own focus-restore so an action that moves focus
    // (e.g. focus composer) lands the caret where it intends, uncontested.
    setTimeout(() => cmd.run(), 0);
  }

  function onInputKeyDown(e: KeyboardEvent) {
    const items = filtered();
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActiveIndex((i) => (items.length === 0 ? 0 : (i + 1) % items.length));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActiveIndex((i) => (items.length === 0 ? 0 : (i - 1 + items.length) % items.length));
    } else if (e.key === "Home") {
      e.preventDefault();
      setActiveIndex(0);
    } else if (e.key === "End") {
      e.preventDefault();
      setActiveIndex(items.length > 0 ? items.length - 1 : 0);
    } else if (e.key === "Enter") {
      const cmd = items[activeIndex()];
      if (cmd) {
        e.preventDefault();
        runCommand(cmd);
      }
    }
    // Escape is left to Kobalte's Dialog to dismiss.
  }

  return (
    <Dialog.Root open={props.open} onOpenChange={props.onOpenChange} modal>
      <Dialog.Portal>
        <Dialog.Overlay class="fixed inset-0 z-50 bg-black/50 data-[expanded]:animate-in data-[closed]:animate-out data-[closed]:fade-out-0 data-[expanded]:fade-in-0" />
        {/* Dynamic viewport units, not `vh`: the palette is a text-entry
            surface, so the on-screen keyboard is up whenever it is used, and
            `14vh + 60vh` measured against the large viewport pushed the bottom
            of the result list under the keyboard on a phone. */}
        <div class="fixed inset-0 z-50 flex items-start justify-center px-4 pt-[14dvh]">
          <Dialog.Content
            class={cn(
              "flex max-h-[60dvh] w-full max-w-[560px] flex-col overflow-hidden rounded-xl border border-border bg-card shadow-lg outline-none",
              "data-[expanded]:animate-in data-[closed]:animate-out data-[closed]:fade-out-0 data-[expanded]:fade-in-0 data-[expanded]:zoom-in-95",
            )}
          >
            <Dialog.Title class="sr-only">Command palette</Dialog.Title>

            {/* Search field — the combobox input. */}
            <div class="flex items-center gap-2 border-b border-border px-3">
              <Search class="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
              <input
                type="text"
                role="combobox"
                aria-expanded="true"
                aria-controls="command-palette-list"
                aria-activedescendant={filtered()[activeIndex()]?.id}
                autocomplete="off"
                autocorrect="off"
                spellcheck={false}
                placeholder="Type a command…"
                class="h-11 w-full bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground"
                value={query()}
                onInput={(e) => {
                  setQuery(e.currentTarget.value);
                  setActiveIndex(0);
                }}
                onKeyDown={onInputKeyDown}
              />
            </div>

            {/* Results — the listbox. */}
            <div id="command-palette-list" role="listbox" class="min-h-0 flex-1 overflow-y-auto p-1.5">
              <Show
                when={filtered().length > 0}
                fallback={
                  <div class="px-3 py-6 text-center text-sm text-muted-foreground">No matching commands</div>
                }
              >
                <For each={filtered()}>
                  {(cmd, i) => (
                    <button
                      type="button"
                      id={cmd.id}
                      role="option"
                      aria-selected={i() === activeIndex()}
                      class={cn(
                        "flex w-full cursor-default items-center gap-2.5 rounded-md px-2.5 py-2 text-left text-sm text-foreground",
                        i() === activeIndex() && "bg-muted",
                      )}
                      onMouseMove={() => setActiveIndex(i())}
                      onClick={() => runCommand(cmd)}
                    >
                      {cmd.icon}
                      <span class="min-w-0 flex-1 truncate">{cmd.label}</span>
                      <Show when={cmd.hint}>
                        <KeyHint keys={cmd.hint!} />
                      </Show>
                    </button>
                  )}
                </For>
              </Show>
            </div>
          </Dialog.Content>
        </div>
      </Dialog.Portal>
    </Dialog.Root>
  );
};

// ---- Shortcut cheat-sheet (`?`) --------------------------------------------

const GROUP_ORDER = ["General", "Tasks", "Focus", "Hirsel"] as const;

export const ShortcutHelp: Component<{
  open: boolean;
  onOpenChange: (open: boolean) => void;
}> = (props) => {
  createOverlayPresence(() => props.open);

  const groups = createMemo(() =>
    GROUP_ORDER.map((group) => ({
      group,
      items: SHORTCUTS.filter((s) => s.group === group),
    })).filter((g) => g.items.length > 0),
  );

  return (
    <Dialog.Root open={props.open} onOpenChange={props.onOpenChange} modal>
      <Dialog.Portal>
        <Dialog.Overlay class="fixed inset-0 z-50 bg-black/50 data-[expanded]:animate-in data-[closed]:animate-out data-[closed]:fade-out-0 data-[expanded]:fade-in-0" />
        <div class="fixed inset-0 z-50 flex items-start justify-center px-4 pt-[14vh]">
          <Dialog.Content
            class={cn(
              "w-full max-w-[420px] overflow-hidden rounded-xl border border-border bg-card p-4 shadow-lg outline-none",
              "data-[expanded]:animate-in data-[closed]:animate-out data-[closed]:fade-out-0 data-[expanded]:fade-in-0 data-[expanded]:zoom-in-95",
            )}
          >
            <Dialog.Title class="m-0 mb-3 text-base font-semibold tracking-[0.01em]">
              Keyboard shortcuts
            </Dialog.Title>
            <div class="flex flex-col gap-4">
              <For each={groups()}>
                {(g) => (
                  <div>
                    <div class="mb-1.5 text-[0.68rem] font-medium uppercase tracking-[0.03em] text-muted-foreground">
                      {g.group}
                    </div>
                    <div class="flex flex-col gap-1">
                      <For each={g.items}>
                        {(s) => (
                          <div class="flex items-center justify-between gap-3 py-0.5">
                            <span class="text-sm text-foreground">{s.label}</span>
                            <KeyHint keys={s.keys} />
                          </div>
                        )}
                      </For>
                    </div>
                  </div>
                )}
              </For>
            </div>
          </Dialog.Content>
        </div>
      </Dialog.Portal>
    </Dialog.Root>
  );
};

// ---- Shared key-chip row ----------------------------------------------------

/** Render keyboard-hint tokens as mono chips. A single token = one key; two
 * tokens render as a chord ("g then i"). */
const KeyHint: Component<{ keys: string[] }> = (props) => (
  <span class="flex shrink-0 items-center gap-1">
    <For each={props.keys}>
      {(k) => (
        <kbd class="grid h-5 min-w-5 place-items-center rounded-sm border border-border bg-muted px-1 font-mono text-meta text-foreground/90">
          {k}
        </kbd>
      )}
    </For>
  </span>
);
