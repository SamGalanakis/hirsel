import { MoreHorizontal, Trash2 } from "lucide-solid";
import { createSignal, For } from "solid-js";
import type { EventItem } from "../../protocol";
import { archiveEventWithUndo } from "../../lib/event-archive";
import { createOverlayPresence } from "../../lib/focus";
import { formatTaskRef } from "../../lib/task-ref";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "../ui/dropdown-menu";
import { taskLabel, taskName, taskStatus, taskTone } from "./task-model";

export function TaskIndex(props: {
  tasks: EventItem[];
  focusedId: number | null;
  onSelect: (task: EventItem) => void;
}) {
  function moveFocus(event: KeyboardEvent, task: EventItem): void {
    const vertical = event.key === "ArrowUp" || event.key === "ArrowDown";
    const horizontal = event.key === "ArrowLeft" || event.key === "ArrowRight";
    const boundary = event.key === "Home" || event.key === "End";
    if (!vertical && !horizontal && !boundary) return;

    event.preventDefault();
    const current = props.tasks.findIndex((item) => item.id === task.id);
    const delta = event.key === "ArrowUp" || event.key === "ArrowLeft" ? -1 : 1;
    const next = event.key === "Home"
      ? 0
      : event.key === "End"
        ? props.tasks.length - 1
        : (current + delta + props.tasks.length) % props.tasks.length;
    const target = props.tasks[next];
    if (!target) return;
    props.onSelect(target);
    queueMicrotask(() => {
      document.querySelector<HTMLButtonElement>(`[data-task-id="${target.id}"]`)?.focus();
    });
  }

  return (
    <nav
      data-slot="task-index"
      aria-label="Tasks"
      class="no-scrollbar flex w-full min-w-0 max-w-full shrink-0 gap-1 overflow-x-auto px-gutter py-2 [contain:inline-size_paint] [mask-image:linear-gradient(90deg,#000_0,#000_92%,transparent)] rail:w-[clamp(210px,19vw,280px)] rail:flex-col rail:overflow-y-auto rail:py-10 rail:[contain:none] rail:[mask-image:none]"
    >
      <For each={props.tasks}>
        {(task) => (
          <TaskChip
            task={task}
            focused={props.focusedId === task.id}
            dimmed={props.focusedId !== null && props.focusedId !== task.id}
            onSelect={() => props.onSelect(task)}
            onKeyDown={(event) => moveFocus(event, task)}
          />
        )}
      </For>
      {/* Trailing room for the ⋯ that floats over the strip's right end at
          phone widths; the rail column has the field's own top-right corner. */}
      <div aria-hidden="true" class="w-10 shrink-0 rail:hidden" />
    </nav>
  );
}

function TaskChip(props: {
  task: EventItem;
  focused: boolean;
  dimmed: boolean;
  onSelect: () => void;
  onKeyDown: (event: KeyboardEvent) => void;
}) {
  // One status decision per chip; the word and the dot are both reads of it.
  const status = () => taskStatus(props.task);
  const label = () => taskLabel(status());

  // The menu is a Kobalte dropdown: it brings its own focus handling and pushes
  // no focus trap, so it has to announce itself to the overlay registry or the
  // global bare-key layer would keep firing underneath it (the same contract
  // the command palette follows).
  const [menuOpen, setMenuOpen] = createSignal(false);
  createOverlayPresence(menuOpen);

  return (
    // The chip and its menu trigger are siblings, never nested: a button inside
    // a button is invalid, and the ⋯ is a second, separately-labelled action on
    // the same row.
    <div class="group/task relative flex shrink-0 rail:w-full">
      <button
        type="button"
        data-task-id={props.task.id}
        aria-pressed={props.focused}
        aria-current={props.focused ? "page" : undefined}
        // Clicking the open chip toggles focus back off (`toggleTaskFocus`), so
        // the promise this name makes is one the chip itself keeps — Esc is the
        // other way out. Clearing focus is deliberately NOT a menu item: the
        // menu is for acting on the Task, not for navigating away from it.
        aria-label={`${taskName(props.task)}, ${label()}, task ${formatTaskRef(props.task.id)}${props.focused ? ", focused; activate to clear focus" : ""}`}
        // The focused chip is marked by a 2px accent rule on the edge it shares
        // with the field it opened — bottom in the horizontal strip, left in the
        // rail column — so the marker points at the instrument. The transparent
        // rule is always present, so nothing shifts when focus moves. Weight and
        // the dimming of the other chips carry the same signal without color
        // (PRODUCT a11y).
        // On a coarse pointer the ⋯ is standing rather than hover-revealed, so
        // the row reserves room for it instead of letting a 44px touch target
        // sit over the task's own name.
        class="flex min-h-11 shrink-0 items-center gap-2.5 rounded-lg border-b-2 border-transparent px-3 text-left text-sm text-muted-foreground outline-none transition-[color,opacity,border-color] hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/60 rail:w-full rail:border-b-0 rail:border-l-2 [@media(pointer:coarse)]:pr-14"
        classList={{
          "border-primary text-foreground font-medium": props.focused,
          "opacity-55": props.dimmed,
        }}
        onClick={props.onSelect}
        onKeyDown={props.onKeyDown}
      >
        <span
          class={`size-1.5 shrink-0 rounded-full ${taskTone(status())}`}
          aria-hidden="true"
        />
        {/* The citation handle, in the one spelling it has everywhere else:
            mono, quiet, no chip or border around it (DESIGN §3 "monospace only
            for machine tokens"). It reads as a column of refs down the rail,
            which is what makes `#12` in a draft findable at a glance. It is
            decorative here — the chip's accessible name already carries it. */}
        <span
          data-slot="task-ref"
          class="shrink-0 font-mono text-meta text-muted-foreground/70"
          aria-hidden="true"
        >
          {formatTaskRef(props.task.id)}
        </span>
        <span class="max-w-48 truncate">{taskName(props.task)}</span>
        <span
          class="ml-auto text-xs text-muted-foreground group-hover/task:invisible group-focus-within/task:invisible [@media(pointer:coarse)]:invisible"
          // The ⋯ takes the status word's place while the row is live. An open
          // menu keeps it hidden explicitly: the menu content is portaled, so
          // `focus-within` no longer holds once focus moves into it.
          classList={{ invisible: menuOpen() }}
        >
          {label()}
        </span>
      </button>
      <DropdownMenu open={menuOpen()} onOpenChange={setMenuOpen} placement="bottom-end" gutter={4}>
        <DropdownMenuTrigger
          data-slot="task-actions-trigger"
          aria-label={`Actions for ${taskName(props.task)}`}
          // Revealed by hover or keyboard focus on a fine pointer, and standing
          // on touch — there is no hover to reveal it with, and Delete has no
          // other way in there.
          class="absolute right-2 top-1/2 grid size-7 -translate-y-1/2 place-items-center rounded-full text-muted-foreground opacity-0 outline-none transition-opacity hover:text-foreground focus-visible:opacity-100 focus-visible:ring-2 focus-visible:ring-ring/60 group-hover/task:opacity-100 group-focus-within/task:opacity-100 data-[expanded]:opacity-100 [@media(pointer:coarse)]:size-11 [@media(pointer:coarse)]:opacity-100"
        >
          <MoreHorizontal class="size-3.5" aria-hidden="true" />
        </DropdownMenuTrigger>
        <DropdownMenuContent class="min-w-[9rem]">
          {/* Nothing red and no confirmation: the archive is optimistic and its
              toast offers an immediate Undo (archive contract v1). */}
          <DropdownMenuItem
            class="[@media(pointer:coarse)]:min-h-11"
            onSelect={() => archiveEventWithUndo(props.task.id)}
          >
            <Trash2 aria-hidden="true" />
            Delete
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}
