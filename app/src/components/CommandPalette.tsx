// Summoned command and shortcut dialogs. Native modal isolation and the shared
// focus stack keep keyboard shortcuts from firing behind the open surface.

import {
  Activity,
  ArrowDownToLine,
  Layers,
  MessagesSquare,
  Minimize2,
  Search,
  Settings,
} from "@/components/ui/icons";
import { type Component, createEffect, createMemo, createSignal, For, Show, onSettled } from "solid-js";
import { type JSX, Portal } from "@solidjs/web";
import { focusThread, threadState } from "../threads/store";
import { threadActions } from "../threads/actions";
import { ThreadActionSymbol } from "../threads/ThreadActions";
import { focusComposer, goPane, jumpToLatest, SHORTCUTS } from "../lib/keymap";
import { createFocusTrap } from "../lib/focus";
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

// ---- Palette ----------------------------------------------------------------

export const CommandPalette: Component<{
  open: boolean;
  onOpenChange: (open: boolean) => void;
}> = (props) => {
  const [query, setQuery] = createSignal("");
  const [activeIndex, setActiveIndex] = createSignal(0);


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
        id: "go-threads",
        label: "Open threads",
        hint: ["g", "t"],
        keywords: "threads work needs you",
        icon: <Layers class={iconClass} aria-hidden="true" />,
        run: () => goPane("threads"),
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
    if (threadState.focusedId !== 0) {
      out.push({
        id: "clear-focus",
        label: "Open Hirsel",
        hint: ["Esc"],
        keywords: "ambient leave exit close unfocus back",
        icon: <Minimize2 class={iconClass} aria-hidden="true" />,
        run: () => focusThread(0),
      });
    }

    const thread = threadState.threads.find(t => t.id === threadState.focusedId);
    if (thread) for (const action of threadActions(thread)) out.push({ id: `${action.id}-thread`, label: action.label, icon: <ThreadActionSymbol name={action.icon} />, run: action.run });

    return out;
  });

  const filtered = createMemo<Command[]>(() => {
    const q = query().trim();
    if (!q) return commands();
    return commands().filter((c) => fuzzyMatch(q, `${c.label} ${c.keywords ?? ""}`));
  });

  // Reset the surface each time it is summoned, and keep the active row in range
  // as the filter narrows.
  createEffect(() => props.open, (open) => {
    if (open) {
      setQuery("");
      setActiveIndex(0);
    }
  });
  createEffect(() => ({ n: filtered().length, index: activeIndex() }), ({ n, index }) => {
    if (index >= n) setActiveIndex(n > 0 ? n - 1 : 0);
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
    // The modal focus stack owns Escape.
  }

  return (
    <ModalSurface open={props.open} onClose={() => props.onOpenChange(false)} label="Command palette"
      class="flex max-h-[60dvh] max-w-[560px] flex-col overflow-hidden">
            <h2 class="sr-only">Command palette</h2>

            {/* Search field — the combobox input. */}
            <div class="flex items-center gap-2 border-b border-border px-3">
              <Search class="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
              <input
                type="text"
                role="combobox"
                aria-label="Search commands"
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
                      tabindex={-1}
                      aria-selected={i() === activeIndex() ? "true" : "false"}
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
    </ModalSurface>
  );
};

// ---- Shortcut cheat-sheet (`?`) --------------------------------------------

const GROUP_ORDER = ["General", "Threads", "Focus", "Hirsel"] satisfies Array<(typeof SHORTCUTS)[number]["group"]>;

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
    <ModalSurface open={props.open} onClose={() => props.onOpenChange(false)} label="Keyboard shortcuts"
      class="max-h-[72dvh] max-w-[420px] overflow-y-auto p-4">
            <div class="mb-3 flex items-center justify-between gap-3">
              <h2 class="m-0 text-base font-semibold tracking-[0.01em]">Keyboard shortcuts</h2>
              <button type="button" class="rounded px-2 py-1 text-sm text-muted-foreground hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring" onClick={() => props.onOpenChange(false)}>Close</button>
            </div>
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
    </ModalSurface>
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

function ModalSurface(props: {open: boolean; onClose: () => void; label: string; class: string; children: JSX.Element}) {
  return <Show when={props.open}><Portal><ModalPanel {...props} /></Portal></Show>;
}
function ModalPanel(props: {onClose: () => void; label: string; class: string; children: JSX.Element}) {
  let panel!: HTMLDialogElement;
  createFocusTrap(() => panel, {onEscape: props.onClose});
  onSettled(() => {
    if (typeof panel.showModal === "function") panel.showModal();
    else panel.setAttribute("open", "");
  });
  return <dialog ref={node => { panel = node; }} aria-label={props.label} aria-modal="true"
    class={cn("fixed inset-x-0 top-[14dvh] bottom-auto mx-auto my-0 w-[calc(100%_-_2rem)] rounded-xl border border-border bg-card text-foreground shadow-lg outline-none backdrop:bg-black/50", props.class)}
    onCancel={(event) => { event.preventDefault(); props.onClose(); }}
    onPointerDown={(event) => {
      if (event.target !== panel) return;
      const bounds = panel.getBoundingClientRect();
      if (event.clientX < bounds.left || event.clientX > bounds.right || event.clientY < bounds.top || event.clientY > bounds.bottom) props.onClose();
    }}>
    {props.children}
  </dialog>;
}
