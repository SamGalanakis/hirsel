import { createMemo, For, Show } from "solid-js";
import { splitTaskRefs } from "../../lib/task-ref";
import { taskEvents } from "../../store/selectors";
import { effectiveEvents, focusTask, state } from "../../store/store";
import { taskName } from "./task-model";

/** The Tasks a citation can resolve to: the resting field, exactly what the
 * chips and the picker show. A ref naming anything else — archived, swept,
 * never seen — resolves to nothing and stays plain text. */
function citableTasks() {
  return taskEvents(effectiveEvents());
}

/** One rendered citation. It is a word in a sentence, not a badge: mono for the
 * ref (DESIGN §3), one quiet underline to say it can be pressed, and no border,
 * fill, pill or status dot around it. The dot is deliberately absent — a
 * coloured dot beside every mention of a Task, mid-sentence, is the
 * "notification slot machine" hirsel is defined against; state belongs on the
 * chip and the card, where it is the subject rather than an aside. Mint is
 * reserved for the interaction: hover, focus, and the Task you are already in.
 *
 * Activating it focuses that Task, which is the only thing a citation could
 * usefully do in a product whose one navigation is focus. */
export function TaskRefTag(props: { taskId: number; token: string }) {
  const task = createMemo(() => citableTasks().find((item) => item.id === props.taskId));
  const current = () => state.focusedTaskId === props.taskId;
  return (
    <Show when={task()} fallback={props.token}>
      {(item) => (
        <button
          type="button"
          data-slot="task-ref-tag"
          data-task-ref={props.taskId}
          aria-current={current() ? "page" : undefined}
          aria-label={`${taskName(item())}, task ${props.token}`}
          title={taskName(item())}
          class="rounded align-baseline text-[0.85em] text-foreground/80 underline decoration-current/25 underline-offset-2 outline-none transition-colors hover:text-primary hover:decoration-current/60 focus-visible:ring-2 focus-visible:ring-ring/60"
          classList={{ "text-primary decoration-current/50": current() }}
          onClick={() => focusTask(props.taskId)}
        >
          {/* The app resets `button { font: inherit }` outside the cascade
              layers, which outranks any font utility ON the button — so the
              monospace lives on the text itself. */}
          <span class="font-mono">{props.token}</span>
        </button>
      )}
    </Show>
  );
}

/** A markdown text run with its Task citations lifted out. Everything that is
 * not a live ref passes through as the literal characters the author typed, so
 * an unknown or archived `#99` degrades to `#99` and nothing breaks. */
export function TaskRefText(props: { value: string }) {
  const spans = createMemo(() => {
    const known = new Set(citableTasks().map((item) => item.id));
    return splitTaskRefs(props.value, (id) => known.has(id));
  });
  return (
    <For each={spans()}>
      {(span) => (
        <Show when={span.taskId !== null} fallback={span.text}>
          <TaskRefTag taskId={span.taskId as number} token={span.text} />
        </Show>
      )}
    </For>
  );
}
