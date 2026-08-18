/*
THESIS: One globally aware Hirsel across a flat field of tasks; tasks are the only destinations.
OWN-WORLD: Warm blue-charcoal, mint interaction, plain task marks, pure-field conversation typography, and almost no frames.
STORY: Open a task, act through its generated instrument, speak within it, then clear focus to return to the ambient whole.
FIRST VIEWPORT: Task index at the left/top, one unfolded JSON-generated task instrument, conversation in its margin, and one standing bottom composer.
FORM: Task Margins, the user-pinned synthesis; focus changes the subject, never the interlocutor.
*/
import { Activity, X } from "lucide-solid";
import { createEffect, createMemo, createSignal, For, on, Show } from "solid-js";
import type { Blob, EventItem, SendMode } from "../../protocol";
import { markEventRead } from "../../lib/event-decide";
import {
  conversationViews,
  orderedTasks,
  runningProcessCount,
  visibleEvents,
} from "../../store/selectors";
import {
  clearComposerPrefill,
  clearProtocolError,
  openProcesses,
  state,
} from "../../store/store";
import { getClient } from "../../ws/client";
import { ViewRenderer } from "../../views/ViewRenderer";
import { BrandMark } from "../BrandMark";
import { ConnectionPill } from "../ConnectionPill";
import { PhoneOverflowMenu } from "../PhoneOverflowMenu";
import { ProcessesSheet } from "../processes/ProcessesSheet";
import { SettingsSheet } from "../settings/SettingsSheet";
import { Composer } from "../chat/Composer";
import { createComposerAttachments } from "../chat/useAttachments";
import { CanvasRail, CanvasSheet } from "../views/CanvasSurface";
import { AmbientField, TaskField } from "./TaskFields";
import { TaskIndex } from "./TaskIndex";
import {
  initialTaskNavigation,
  reconcileTaskNavigation,
  toggleTaskFocus,
} from "./task-navigation";
import { taskSendContext } from "./task-model";

export function TaskShell() {
  const attachments = createComposerAttachments();
  const [navigation, setNavigation] = createSignal(initialTaskNavigation);
  let taskScrollRef: HTMLDivElement | undefined;

  const visible = createMemo(() => visibleEvents(state.events, state.eventArchiveOverrides));
  const tasks = createMemo(() => orderedTasks(visible(), state.eventDecideOverrides));
  const processCount = () => runningProcessCount(state.processes);
  const focusedTask = createMemo(() =>
    tasks().find((task) => task.id === navigation().focusedId) ?? null
  );
  const taskViews = createMemo(() => {
    const task = focusedTask();
    if (!task) return [];
    // `ping:<id>` is retained only as the backend wire placement for a Task.
    return state.views.filter((view) => view.placement === `ping:${task.id}`);
  });

  createEffect(() => setNavigation((current) => reconcileTaskNavigation(current, tasks())));

  createEffect(on(() => navigation().focusedId, () => {
    if (taskScrollRef) taskScrollRef.scrollTop = 0;
  }));

  function selectTask(task: EventItem): void {
    setNavigation((current) => toggleTaskFocus(current, task));
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
    <div data-slot="task-shell" class="mx-auto flex min-h-0 w-full max-w-[1600px] flex-1 flex-col overflow-hidden">
      <header class="flex h-14 shrink-0 items-center gap-3 px-4 rail:px-6">
        <div class="flex items-center gap-2">
          <BrandMark size={20} />
          <h1 class="m-0 text-sm font-semibold tracking-[0.01em]">hirsel</h1>
        </div>
        <div class="ml-auto"><ConnectionPill compact /></div>
        <button
          type="button"
          data-slot="processes-trigger"
          aria-label={
            processCount() > 0
              ? `Processes, ${processCount()} running`
              : "Processes"
          }
          aria-pressed={state.rightRegion === "processes"}
          class="flex min-h-11 shrink-0 items-center gap-1.5 rounded-lg px-2 text-xs text-muted-foreground outline-none transition-colors hover:bg-muted/60 hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/60 aria-pressed:bg-muted aria-pressed:text-foreground"
          onClick={openProcesses}
        >
          <Activity class="size-4" aria-hidden="true" />
          <span class="hidden rail:inline">Processes</span>
          <Show when={processCount() > 0}>
            <span class="min-w-4 text-center font-semibold text-status-active" aria-hidden="true">
              {processCount() > 99 ? "99+" : processCount()}
            </span>
          </Show>
        </button>
        <PhoneOverflowMenu />
      </header>

      <div class="flex min-h-0 flex-1 flex-col rail:flex-row">
        <TaskIndex
          tasks={tasks()}
          focusedId={navigation().focusedId}
          decideOverrides={state.eventDecideOverrides}
          onSelect={selectTask}
        />
        <main class="relative flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
          <div ref={(node) => { taskScrollRef = node; }} data-slot="task-scroll" class="min-h-0 flex-1 overflow-y-auto">
            <Show when={focusedTask()} fallback={<AmbientField />}>
              {(task) => <TaskField task={task()} tasks={tasks()} views={taskViews()} />}
            </Show>
            <Show when={!focusedTask() && conversationViews(state.views).length > 0}>
              <div class="mx-auto flex w-full max-w-[760px] flex-col gap-4 px-6 pb-12">
                <For each={conversationViews(state.views)}>
                  {(view) => <ViewRenderer spec={view.spec} instanceId={view.instance_id} placement={view.placement} />}
                </For>
              </div>
            </Show>
          </div>

          <Show when={state.protocolError}>
            <div role="alert" class="mx-4 mb-2 flex items-center gap-3 rounded-lg bg-muted px-3 text-sm text-foreground">
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
