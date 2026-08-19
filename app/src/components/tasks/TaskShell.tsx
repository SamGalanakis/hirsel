/*
THESIS: One globally aware Hirsel across a flat field of tasks; tasks are the only destinations.
OWN-WORLD: Warm blue-charcoal, mint interaction, plain task marks, pure-field conversation typography, and almost no frames.
STORY: Open a task, act through its generated instrument, speak within it, then clear focus to return to the ambient whole.
FIRST VIEWPORT: Task index at the left/top, one unfolded JSON-generated task instrument, conversation in its margin, and one standing bottom composer.
FORM: Task Margins, the user-pinned synthesis; focus changes the subject, never the interlocutor.
*/
import { X } from "lucide-solid";
import { createEffect, createMemo, For, on, Show } from "solid-js";
import type { Blob, EventItem, SendMode } from "../../protocol";
import { markEventRead } from "../../lib/event-decide";
import {
  conversationViews,
  orderedTasks,
  visibleEvents,
} from "../../store/selectors";
import {
  clearComposerPrefill,
  clearProtocolError,
  clearTaskFocus,
  focusTask,
  reconcileTaskFocus,
  state,
  toggleTaskFocus,
} from "../../store/store";
import { getClient } from "../../ws/client";
import { ViewRenderer } from "../../views/ViewRenderer";
import { ConnectionPill, connectionLabel } from "../ConnectionPill";
import { PhoneOverflowMenu } from "../PhoneOverflowMenu";
import { ProcessesSheet } from "../processes/ProcessesSheet";
import { SettingsSheet } from "../settings/SettingsSheet";
import { Composer } from "../chat/Composer";
import { createComposerAttachments } from "../chat/useAttachments";
import { CanvasRail, CanvasSheet } from "../views/CanvasSurface";
import { AmbientField, TaskField } from "./TaskFields";
import { TaskIndex } from "./TaskIndex";
import { mostNeedingTask, taskSendContext } from "./task-model";

export function TaskShell() {
  const attachments = createComposerAttachments();
  let taskScrollRef: HTMLDivElement | undefined;

  const visible = createMemo(() => visibleEvents(state.events, state.eventArchiveOverrides));
  const tasks = createMemo(() => orderedTasks(visible(), state.eventDecideOverrides));
  const focusedTask = createMemo(() =>
    tasks().find((task) => task.id === state.focusedTaskId) ?? null
  );
  const taskViews = createMemo(() => {
    const task = focusedTask();
    if (!task) return [];
    // `ping:<id>` is retained only as the backend wire placement for a Task.
    return state.views.filter((view) => view.placement === `ping:${task.id}`);
  });

  createEffect(() => reconcileTaskFocus(tasks().map((task) => task.id)));

  // Open focused by default. The field hydrates with the handshake (`hello_ok`
  // carries the whole Task set and is dispatched immediately before the socket
  // reports `connected`), so the first "connected" is the one honest moment at
  // which "what needs me most right now?" has an answer.
  //
  // It fires exactly ONCE per load, whatever happens afterwards: a reconnect
  // re-enters "connected" and must not re-subject the field, and a Task
  // arriving while the Owner sits in the ambient field never steals focus —
  // ambient is a deliberate state (Esc), not an empty one waiting to be filled.
  let autoFocusSettled = false;
  createEffect(on(() => state.connection, (connection) => {
    if (autoFocusSettled || connection !== "connected") return;
    autoFocusSettled = true;
    if (state.focusedTaskId !== null) return; // a focus already chosen wins
    const task = mostNeedingTask(tasks(), state.eventDecideOverrides);
    if (task) focusTask(task.id);
  }));

  // A focus change re-subjects the whole field: start the new task at its top,
  // and bring its chip into view so the marker and the instrument it opened are
  // never in two different places (the strip in particular scrolls
  // independently and would otherwise leave the marker off-screen).
  createEffect(on(() => state.focusedTaskId, (focusedId) => {
    if (taskScrollRef) taskScrollRef.scrollTop = 0;
    if (focusedId === null) return;
    const chip = document.querySelector<HTMLElement>(`[data-task-id="${focusedId}"]`);
    chip?.scrollIntoView?.({ inline: "center", block: "nearest" });
  }));

  function selectTask(task: EventItem): void {
    toggleTaskFocus(task.id);
    if (!task.read) markEventRead(task.id);
  }

  function send(
    body: string,
    ref: number | null,
    mode: SendMode,
    blobs: Blob[],
    mentions: number[],
  ): void {
    const context = taskSendContext(focusedTask(), ref, mentions);
    getClient()?.sendMessage(body, context.ref, {
      mode,
      attachments: blobs,
      mentions: context.mentions,
    });
  }

  function lastOwnerBody(): string | null {
    for (let i = state.messages.length - 1; i >= 0; i--) {
      if (state.messages[i].author === "owner") return state.messages[i].body;
    }
    return null;
  }

  return (
    <div data-slot="task-shell" class="relative mx-auto flex min-h-0 w-full max-w-[1600px] flex-1 flex-col overflow-hidden">
      {/* Home has no header bar. The brandmark, the standing connection dot and
          the Processes button were chrome the Owner reads past every time; the
          top anchor is now the content itself. What survives floats over the
          field: one quiet ⋯ (every utility, including Processes, lives behind
          it) and connection state ONLY while it is abnormal. Utility panes keep
          their own PaneHeader — this collapse is the home shell's alone. */}
      <div
        data-slot="home-affordances"
        class="pointer-events-none absolute inset-x-0 top-0 z-20 flex items-start gap-2 px-gutter pt-[calc(env(safe-area-inset-top)+0.5rem)]"
      >
        {/* Silence is the healthy state (PRODUCT "restraint as respect"): a
            connected socket shows nothing at all. Announcement is NOT silent
            though — one never-unmounted live region speaks every transition,
            including the recovery the visible pill can't report because it has
            already left the screen. The pill itself is then decorative. */}
        <span data-slot="connection-status" class="sr-only" role="status" aria-live="polite">
          {connectionLabel(state.connection)}
        </span>
        <Show when={state.connection !== "connected"}>
          <div class="pointer-events-auto" aria-hidden="true"><ConnectionPill /></div>
        </Show>
        <div class="pointer-events-auto ml-auto"><PhoneOverflowMenu /></div>
      </div>

      <div class="flex min-h-0 flex-1 flex-col rail:flex-row">
        <TaskIndex
          tasks={tasks()}
          focusedId={state.focusedTaskId}
          decideOverrides={state.eventDecideOverrides}
          onSelect={selectTask}
          onClearFocus={clearTaskFocus}
        />
        <main class="relative flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
          {/* The field is anchored to the composer, not to the top bar: short
              content rests just above the composer (its children carry
              `min-h-full` + end-alignment), long content still scrolls from the
              top because it simply outgrows the container. */}
          <div
            ref={(node) => { taskScrollRef = node; }}
            data-slot="task-scroll"
            class="flex min-h-0 flex-1 flex-col justify-end overflow-y-auto"
          >
            <Show when={focusedTask()} fallback={<AmbientField />}>
              {(task) => <TaskField task={task()} tasks={tasks()} views={taskViews()} />}
            </Show>
            <Show when={!focusedTask() && conversationViews(state.views).length > 0}>
              <div class="mx-auto w-full max-w-frame px-gutter pb-12">
                <div class="flex w-full max-w-measure flex-col gap-4">
                  <For each={conversationViews(state.views)}>
                    {(view) => (
                      <ViewRenderer spec={view.spec} instanceId={view.instance_id} placement={view.placement} />
                    )}
                  </For>
                </div>
              </div>
            </Show>
          </div>

          <Show when={state.protocolError}>
            <div role="alert" class="mx-gutter mb-2 flex items-center gap-3 rounded-lg bg-muted px-3 text-sm text-foreground">
              <span class="min-w-0 flex-1 py-3">{state.protocolError}</span>
              <button
                type="button"
                class="grid size-11 shrink-0 place-items-center rounded-lg text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/60"
                aria-label="Dismiss connection error"
                onClick={clearProtocolError}
              >
                <X class="size-4" aria-hidden="true" />
              </button>
            </div>
          </Show>

          <Composer
            focused={focusedTask() !== null}
            attachments={attachments}
            thinking={state.agentActivity.state === "thinking"}
            prefill={state.composerPrefill}
            onConsumePrefill={clearComposerPrefill}
            onSend={send}
            onStop={() => getClient()?.cancelTurn()}
            getLastOwnerBody={lastOwnerBody}
          />
        </main>

        <CanvasRail />
        <CanvasSheet />
        <ProcessesSheet />
        <SettingsSheet />
      </div>
    </div>
  );
}
