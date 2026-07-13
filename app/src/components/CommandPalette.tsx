// The ⌘K command palette + the `?` shortcut cheat-sheet — two summoned, never
// standing surfaces. Both are calm Kobalte `Dialog`s (portal + scrim + focus
// trap + Esc-to-dismiss, so they cleanly participate in `anyOverlayOpen`). The
// palette is a filterable command list (a combobox/listbox pattern: a search
// field driving a `role="listbox"` with `aria-activedescendant`), styled to the
// calm-terminal register — Inter for labels, mono only for the keyboard hints.

import * as Dialog from "@kobalte/core/dialog";
import {
  Activity,
  ArrowDownToLine,
  AtSign,
  CircleStop,
  Inbox as InboxIcon,
  MessageSquareReply,
  MessagesSquare,
  Search,
  Settings,
} from "lucide-solid";
import { type Component, createEffect, createMemo, createSignal, For, type JSX, Show } from "solid-js";
import { goToChat, openSideChat, state } from "../store/store";
import { focusComposer, goPane, jumpToLatest, SHORTCUTS, stopActiveTurn } from "../lib/keymap";
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

// ---- Palette ----------------------------------------------------------------

export const CommandPalette: Component<{
  open: boolean;
  onOpenChange: (open: boolean) => void;
}> = (props) => {
  const [query, setQuery] = createSignal("");
  const [activeIndex, setActiveIndex] = createSignal(0);

  const thinking = () => state.agentActivity.state === "thinking";
  const iconClass = "size-4 shrink-0 text-muted-foreground";

  // The full command set, rebuilt reactively so the dynamic entries (open Pings,
  // live Side Chats, the stop-turn action) track store state.
  const commands = createMemo<Command[]>(() => {
    const out: Command[] = [
      {
        id: "focus-composer",
        label: "Focus composer",
        hint: ["/"],
        keywords: "type write message reply",
        icon: <MessagesSquare class={iconClass} aria-hidden="true" />,
        run: focusComposer,
      },
      {
        id: "go-chat",
        label: "Go to Chat",
        hint: ["g", "c"],
        icon: <MessagesSquare class={iconClass} aria-hidden="true" />,
        run: () => goPane("chat"),
      },
      {
        id: "go-pings",
        label: "Open Pings",
        hint: ["g", "i"],
        keywords: "pings inbox tray",
        icon: <InboxIcon class={iconClass} aria-hidden="true" />,
        run: () => goPane("pings"),
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

    if (thinking()) {
      out.push({
        id: "stop-turn",
        label: "Stop the active turn",
        keywords: "cancel halt interrupt",
        icon: <CircleStop class={iconClass} aria-hidden="true" />,
        run: stopActiveTurn,
      });
    }

    // Jump to an @ping — every open Ping, addressed by its handle.
    for (const ping of state.pings) {
      if (ping.status !== "open") continue;
      out.push({
        id: `ping-${ping.id}`,
        label: `Jump to @${ping.name}`,
        keywords: `ping ${ping.name} ${ping.description}`,
        icon: <AtSign class={iconClass} aria-hidden="true" />,
        run: () => goToChat({ scrollToMessageId: ping.anchor }),
      });
    }

    // Resume a live Side Chat, labelled by its originating Ping's handle.
    for (const ref of state.sideChatRefs) {
      const ping = state.pings.find((p) => p.id === ref.ping_id);
      const name = ping ? `@${ping.name}` : "side chat";
      out.push({
        id: `sc-${ref.sc}`,
        label: `Resume ${name}`,
        keywords: `side chat resume ${ping?.name ?? ""}`,
        icon: <MessageSquareReply class={iconClass} aria-hidden="true" />,
        run: () => openSideChat(ref.sc),
      });
    }

    return out;
  });

  const filtered = createMemo<Command[]>(() => {
    const q = query().trim().toLowerCase();
    if (!q) return commands();
    return commands().filter((c) => `${c.label} ${c.keywords ?? ""}`.toLowerCase().includes(q));
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
        <div class="fixed inset-0 z-50 flex items-start justify-center px-4 pt-[14vh]">
          <Dialog.Content
            class={cn(
              "flex max-h-[60vh] w-full max-w-[560px] flex-col overflow-hidden rounded-xl border border-border bg-card shadow-lg outline-none",
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
            <ul id="command-palette-list" role="listbox" class="min-h-0 flex-1 overflow-y-auto p-1.5">
              <Show
                when={filtered().length > 0}
                fallback={
                  <li class="px-3 py-6 text-center text-sm text-muted-foreground">No matching commands</li>
                }
              >
                <For each={filtered()}>
                  {(cmd, i) => (
                    <li
                      id={cmd.id}
                      role="option"
                      aria-selected={i() === activeIndex()}
                      class={cn(
                        "flex cursor-default items-center gap-2.5 rounded-md px-2.5 py-2 text-sm text-foreground",
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
                    </li>
                  )}
                </For>
              </Show>
            </ul>
          </Dialog.Content>
        </div>
      </Dialog.Portal>
    </Dialog.Root>
  );
};

// ---- Shortcut cheat-sheet (`?`) --------------------------------------------

const GROUP_ORDER = ["General", "Navigate", "Chat"] as const;

export const ShortcutHelp: Component<{
  open: boolean;
  onOpenChange: (open: boolean) => void;
}> = (props) => {
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
        <kbd class="grid h-5 min-w-5 place-items-center rounded-sm border border-border bg-muted px-1 font-mono text-[0.72rem] text-foreground/90">
          {k}
        </kbd>
      )}
    </For>
  </span>
);
