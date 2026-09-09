import { For, Show } from "solid-js";

import type { RefTarget } from "../../lib/task-ref";
import { formatTaskRef } from "../../lib/task-ref";


export const TASK_REF_PICKER_ID = "task-ref-picker";

/** The inline Task picker the `#` trigger opens, anchored under the `#` being
 * typed. It is a summoned surface, so it takes the quiet-field tone and one
 * hairline (DESIGN §2) and nothing else: no header, no count, no empty state —
 * it does not exist when it has nothing to offer.
 *
 * Focus never leaves the composer. The rows are `option`s under an
 * `aria-activedescendant` on the textarea, so the keyboard stays where the Owner
 * is typing; on a coarse pointer each row is a 44px target and a tap accepts it
 * without dropping the keyboard. */
export function TaskRefPicker(props: {
  candidates: RefTarget[];
  activeIndex: number;
  anchorX: number;
  onAccept: (task: RefTarget) => void;
  onHover: (index: number) => void;
}) {
  return (
    <Show when={props.candidates.length > 0}>
      <div
        data-slot="task-ref-picker"
        id={TASK_REF_PICKER_ID}
        role="listbox"
        aria-label="Cite a thread"
        /* Anchored to the caret, clamped so a `#` typed at the right-hand end of
           a long line never pushes the list off the column. */
        style={{
          left: `max(0px, min(${props.anchorX}px, calc(100% - 17rem)))`,
        }}
        class="absolute bottom-[calc(100%+0.75rem)] z-30 w-[17rem] max-w-full overflow-hidden rounded-2xl border border-border/60 bg-surface p-1 shadow-sm"
      >
        <For each={props.candidates}>
          {(task, index) => {
            const status = () => task.settled_at ? "Settled" : task.attention === "needs_owner" ? "Needs you" : "";
            return (
              <div
                id={`${TASK_REF_PICKER_ID}-option-${task.id}`}
                role="option"
                /* The rows never take focus — the caret stays in the composer
                   and `aria-activedescendant` does the pointing — but an option
                   must still be focusable to be a legal one. */
                tabindex={-1}
                data-task-ref-option={task.id}
                aria-selected={(index() === props.activeIndex) ? "true" : "false"}
                class={["flex cursor-pointer items-center gap-2.5 rounded-xl px-2 py-1.5 text-sm text-muted-foreground [@media(pointer:coarse)]:min-h-11", { "bg-primary/10 text-foreground": index() === props.activeIndex }]}

                onMouseEnter={() => props.onHover(index())}
                /* Pointer-down, not click: a click would blur the composer
                   first, and the caret the insertion needs would be gone. */
                onPointerDown={(event) => {
                  event.preventDefault();
                  props.onAccept(task);
                }}
              >
                <span class="size-1.5 shrink-0 rounded-full bg-muted-foreground" aria-hidden="true" />
                <span class="shrink-0 font-mono text-meta text-muted-foreground/80">
                  {formatTaskRef(task.id)}
                </span>
                <span class="min-w-0 flex-1 truncate">{task.title ?? task.name}</span>
                <span class="shrink-0 text-xs text-muted-foreground/70">{status()}</span>
              </div>
            );
          }}
        </For>
      </div>
    </Show>
  );
}
