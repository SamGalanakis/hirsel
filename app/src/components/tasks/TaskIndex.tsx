import { X } from "lucide-solid";
import { For, Show } from "solid-js";
import type { EventItem } from "../../protocol";
import { taskName, taskState, taskTone } from "./task-model";

export function TaskIndex(props: {
  tasks: EventItem[];
  focusedId: number | null;
  decideOverrides: number[];
  onSelect: (task: EventItem) => void;
  onClearFocus: () => void;
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
        {(task) => {
          const focused = () => props.focusedId === task.id;
          const dimmed = () => props.focusedId !== null && !focused();
          const status = () => taskState(task, props.decideOverrides);
          return (
            // The chip and its exit are siblings, never nested: a button inside a
            // button is invalid, and the × is a second, separately-labelled
            // action on the same row.
            <div class="group/task relative flex shrink-0 rail:w-full">
              <button
                type="button"
                data-task-id={task.id}
                aria-pressed={focused()}
                aria-current={focused() ? "page" : undefined}
                aria-label={`${taskName(task)}, ${status()}${focused() ? ", focused; activate to clear focus" : ""}`}
                // The focused chip is marked by a 2px accent rule on the edge it
                // shares with the field it opened — bottom in the horizontal
                // strip, left in the rail column — so the marker points at the
                // instrument. The transparent rule is always present, so nothing
                // shifts when focus moves. Weight and the dimming of the other
                // chips carry the same signal without color (PRODUCT a11y).
                class="flex min-h-11 shrink-0 items-center gap-2.5 rounded-lg border-b-2 border-transparent px-3 text-left text-sm text-muted-foreground outline-none transition-[color,opacity,border-color] hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/60 rail:w-full rail:border-b-0 rail:border-l-2"
                classList={{
                  "border-primary text-foreground font-medium": focused(),
                  "opacity-55": dimmed(),
                }}
                onClick={() => props.onSelect(task)}
                onKeyDown={(event) => moveFocus(event, task)}
              >
                <span
                  class={`size-1.5 shrink-0 rounded-full ${taskTone(task, props.decideOverrides)}`}
                  aria-hidden="true"
                />
                <span class="max-w-48 truncate">{taskName(task)}</span>
                <span
                  class="ml-auto text-xs text-muted-foreground"
                  // On a fine pointer the × takes the status word's place while
                  // the row is live; touch keeps the word, since there is no
                  // hover to reveal the affordance with.
                  classList={{
                    "group-hover/task:invisible group-focus-within/task:invisible [@media(pointer:coarse)]:visible":
                      focused(),
                  }}
                >
                  {status()}
                </span>
              </button>
              <Show when={focused()}>
                <button
                  type="button"
                  aria-label="Clear focus"
                  class="absolute right-2 top-1/2 grid size-7 -translate-y-1/2 place-items-center rounded-full text-muted-foreground opacity-0 outline-none transition-opacity hover:text-foreground focus-visible:opacity-100 focus-visible:ring-2 focus-visible:ring-ring/60 group-hover/task:opacity-100 group-focus-within/task:opacity-100 [@media(pointer:coarse)]:hidden"
                  onClick={props.onClearFocus}
                >
                  <X class="size-3.5" aria-hidden="true" />
                </button>
              </Show>
            </div>
          );
        }}
      </For>
    </nav>
  );
}
