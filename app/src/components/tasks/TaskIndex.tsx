import { For } from "solid-js";
import type { EventItem } from "../../protocol";
import { taskName, taskState, taskTone } from "./task-model";

export function TaskIndex(props: {
  tasks: EventItem[];
  focusedId: number | null;
  decideOverrides: number[];
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
      class="no-scrollbar flex w-full min-w-0 max-w-full shrink-0 gap-1 overflow-x-auto px-3 py-2 [contain:inline-size_paint] [mask-image:linear-gradient(90deg,#000_0,#000_92%,transparent)] rail:w-[clamp(210px,19vw,280px)] rail:flex-col rail:overflow-y-auto rail:px-4 rail:py-10 rail:[contain:none] rail:[mask-image:none]"
    >
      <For each={props.tasks}>
        {(task) => {
          const focused = () => props.focusedId === task.id;
          const status = () => taskState(task, props.decideOverrides);
          return (
            <button
              type="button"
              data-task-id={task.id}
              aria-pressed={focused()}
              aria-label={`${taskName(task)}, ${status()}${focused() ? ", focused; activate to clear focus" : ""}`}
              class="group flex min-h-11 shrink-0 items-center gap-2.5 rounded-lg px-3 text-left text-sm text-muted-foreground outline-none transition-[background,color] hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/60 rail:w-full"
              classList={{ "bg-primary/[0.08] text-primary": focused() }}
              onClick={() => props.onSelect(task)}
              onKeyDown={(event) => moveFocus(event, task)}
            >
              <span
                class={`size-1.5 shrink-0 rounded-full ${taskTone(task, props.decideOverrides)}`}
                classList={{ "ring-4 ring-primary/10": focused() }}
                aria-hidden="true"
              />
              <span class="max-w-48 truncate">{taskName(task)}</span>
              <span class="ml-auto text-xs text-muted-foreground">{status()}</span>
            </button>
          );
        }}
      </For>
    </nav>
  );
}
