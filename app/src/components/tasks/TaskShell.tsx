/*
THESIS: One globally aware Hirsel across a flat field of tasks; tasks are the only destinations.
OWN-WORLD: Warm blue-charcoal, mint interaction, plain task marks, pure-field conversation typography, and almost no frames.
STORY: Open a task, act through its generated instrument, speak within it, then clear focus to return to the ambient whole.
FIRST VIEWPORT: Task index at the left/top, one unfolded JSON-generated task instrument, conversation in its margin, and one standing bottom composer.
FORM: Task Margins, the user-pinned synthesis; focus changes the subject, never the interlocutor.
*/
import { ArrowDown, X } from "lucide-solid";
import { createEffect, createMemo, createSignal, For, on, Show } from "solid-js";
import type { Blob, EventItem, SendMode } from "../../protocol";
import { markEventRead } from "../../lib/event-decide";
import {
  conversationViews,
  orderedTasks,
  taskEvents,
} from "../../store/selectors";
import {
  clearComposerPrefill,
  clearProtocolError,
  clearTaskFocus,
  dispatch,
  effectiveEvents,
  focusTask,
  reconcileTaskFocus,
  setProtocolError,
  state,
  toggleTaskFocus,
} from "../../store/store";
import { isAtBottom, scrollToBottom, shouldOfferJump } from "../../lib/scroll";
import {
  CONVERSATION_WINDOW,
  HISTORY_PAGE_SIZE,
  capturePrependAnchor,
  decideScrollback,
  outranHistoryPrefetch,
  restorePrependAnchor,
} from "../../lib/scrollback";
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
import { messagesForTask, mostNeedingTask, taskSendContext } from "./task-model";

export function TaskShell() {
  const attachments = createComposerAttachments();
  let taskScrollRef: HTMLDivElement | undefined;
  const [revealed, setRevealed] = createSignal(CONVERSATION_WINDOW);
  const [loadingEarlier, setLoadingEarlier] = createSignal(false);
  let backfillInFlight = false;
  let earlierLoad: Promise<void> | null = null;
  let scrollbackMutationScheduled = false;

  // Follow-at-bottom. `following` is a plain mirror, deliberately NOT a signal
  // the follow effect reads: the effect must react to conversation growth only,
  // never to its own scrolling. `canJump` is the rendered half.
  let following = true;
  const [canJump, setCanJump] = createSignal(false);
  // When the last programmatic pin started. A smooth jump emits scroll events
  // all the way down, and each one measures as "not at the bottom yet"; without
  // this the affordance would flicker back on for the length of the animation.
  // A timestamp rather than a flag so an interrupted animation heals itself
  // instead of latching.
  let pinnedAt = 0;
  const PIN_SETTLE_MS = 700;

  function measure(): void {
    const element = taskScrollRef;
    if (!element) return;
    const geometry = {
      scrollTop: element.scrollTop,
      clientHeight: element.clientHeight,
      scrollHeight: element.scrollHeight,
    };
    const atBottom = isAtBottom(geometry);
    if (!atBottom && Date.now() - pinnedAt < PIN_SETTLE_MS) return;
    following = atBottom;
    setCanJump(state.hasLaterMessages || shouldOfferJump(geometry));
    setLoadingEarlier(backfillInFlight && outranHistoryPrefetch(geometry));
    advanceScrollback(geometry);
  }

  function pin(instant = true): void {
    const element = taskScrollRef;
    if (!element) return;
    pinnedAt = Date.now();
    scrollToBottom(element, instant);
    following = true;
    setCanJump(false);
  }

  // One scalar that changes on every kind of conversation growth: a new or
  // edited message, another streamed delta, or the turn's liveness flipping.
  // Streaming appends a fresh `turnEvents` entry per delta, so its length moves
  // with the text.
  const growth = createMemo(() => {
    const first = state.messages.find((message) => !message.pending);
    const last = state.messages[state.messages.length - 1];
    return [
      state.messages.length,
      first?.id ?? 0,
      last?.id ?? 0,
      last?.body.length ?? 0,
      state.turnEvents.length,
      state.agentActivity.state,
      state.hasEarlierMessages,
      state.hasLaterMessages,
    ].join(":");
  });

  // The whole rule: growth scrolls down only for a reader already at the
  // bottom. A reader who scrolled up is reading, and is never yanked — the jump
  // affordance is their way back. Deferred so mounting doesn't count as growth,
  // and pinned on the next frame so the measurement sees the grown content.
  createEffect(
    on(growth, () => {
      if (!taskScrollRef) return;
      if (state.hasLaterMessages) {
        following = false;
        setCanJump(true);
        return;
      }
      if (!following) {
        measure();
        return;
      }
      requestAnimationFrame(() => pin(true));
    }, { defer: true }),
  );

  // The field's whole subject matter: the resting Task set (housekeeping info
  // events excluded by `taskEvents`), in queue order.
  const tasks = createMemo(() => orderedTasks(taskEvents(effectiveEvents())));
  const focusedTask = createMemo(() =>
    tasks().find((task) => task.id === state.focusedTaskId) ?? null
  );
  const activeMessages = createMemo(() => {
    const task = focusedTask();
    return task ? messagesForTask(task, state.messages, tasks()) : state.messages;
  });
  const taskViews = createMemo(() => {
    const task = focusedTask();
    if (!task) return [];
    // `ping:<id>` is retained only as the backend wire placement for a Task.
    return state.views.filter((view) => view.placement === `ping:${task.id}`);
  });

  createEffect(() => reconcileTaskFocus(tasks().map((task) => task.id)));

  function finishAnchoredMutation(anchor: ReturnType<typeof capturePrependAnchor>): void {
    queueMicrotask(() => {
      requestAnimationFrame(() => {
        if (taskScrollRef) restorePrependAnchor(taskScrollRef, anchor);
        scrollbackMutationScheduled = false;
        measure();
      });
    });
  }

  function revealEarlier(count: number): void {
    const element = taskScrollRef;
    if (!element || count <= 0 || scrollbackMutationScheduled) return;
    scrollbackMutationScheduled = true;
    const anchor = capturePrependAnchor(element);
    setRevealed((current) => current + count);
    finishAnchoredMutation(anchor);
  }

  function requestEarlier(): void {
    if (backfillInFlight) return;
    const client = getClient();
    const beforeId = state.messages.find((message) => !message.pending && message.id > 0)?.id;
    if (!client || beforeId === undefined) return;

    backfillInFlight = true;
    if (taskScrollRef) {
      const geometry = {
        scrollTop: taskScrollRef.scrollTop,
        clientHeight: taskScrollRef.clientHeight,
        scrollHeight: taskScrollRef.scrollHeight,
      };
      setLoadingEarlier(outranHistoryPrefetch(geometry));
    }

    earlierLoad = client
      .fetchMessages(beforeId, HISTORY_PAGE_SIZE)
      .then((page) => {
        const element = taskScrollRef;
        const anchor = element ? capturePrependAnchor(element) : null;
        const beforeCount = activeMessages().length;
        dispatch({ type: "messages_page", payload: page, placement: "earlier" });
        const added = Math.max(0, activeMessages().length - beforeCount);
        if (added > 0) setRevealed((current) => current + Math.min(CONVERSATION_WINDOW, added));
        if (anchor) {
          scrollbackMutationScheduled = true;
          finishAnchoredMutation(anchor);
        }
      })
      .catch((error: unknown) => {
        const detail = error instanceof Error ? error.message : String(error);
        setProtocolError(`Couldn’t load earlier messages: ${detail}`);
      })
      .finally(() => {
        backfillInFlight = false;
        setLoadingEarlier(false);
        earlierLoad = null;
        requestAnimationFrame(() => measure());
      });
  }

  function advanceScrollback(geometry: {
    scrollTop: number;
    clientHeight: number;
    scrollHeight: number;
  }): void {
    if (scrollbackMutationScheduled) return;
    const loaded = activeMessages().length;
    const decision = decideScrollback({
      geometry,
      rendered: Math.min(revealed(), loaded),
      loaded,
      hasEarlier: state.hasEarlierMessages,
      backfillInFlight,
    });
    if (decision.reveal > 0) revealEarlier(decision.reveal);
    else if (decision.fetchBeforeOldest) requestEarlier();
  }

  async function jumpLatest(instant = false): Promise<void> {
    if (!state.hasLaterMessages) {
      pin(instant);
      return;
    }
    if (earlierLoad) await earlierLoad;
    const client = getClient();
    const latest = state.lastSeenMsgId;
    if (!client || latest === null) {
      pin(instant);
      return;
    }
    try {
      const page = await client.fetchMessages(latest + 1, HISTORY_PAGE_SIZE);
      dispatch({ type: "messages_page", payload: page, placement: "latest" });
      setRevealed(CONVERSATION_WINDOW);
      requestAnimationFrame(() => pin(instant));
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      setProtocolError(`Couldn’t load the latest messages: ${detail}`);
    }
  }

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
    const task = mostNeedingTask(tasks());
    if (task) focusTask(task.id);
  }));

  // A focus change re-subjects the whole field: start at the newest line and
  // bring the new task's chip into view, so the marker and the instrument it
  // opened are never in two different places (the strip in particular scrolls
  // independently and would otherwise leave the marker off-screen).
  //
  // BOTH states start at the bottom now. A focused task used to open at
  // scrollTop 0 because the instrument sat in its own column and scrolling
  // away from it lost the subject. The instrument is a PINNED card at the top
  // of the same column now, visible at every scroll offset, so there is
  // nothing left to protect by holding the field at its top — and every state
  // opens where a conversation opens, at its newest line above the composer.
  createEffect(on(() => state.focusedTaskId, (focusedId) => {
    setRevealed(CONVERSATION_WINDOW);
    void jumpLatest(true);
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
          onSelect={selectTask}
          onClearFocus={clearTaskFocus}
        />
        <main class="relative flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
          {/* Scrolled text never hard-clips against the top edge: a short
              background→transparent veil fades it out instead. An overlay
              rather than a mask on the scroller so the focused state's sticky
              card (itself bg-background) passes under it invisibly. */}
          <div
            aria-hidden="true"
            class="pointer-events-none absolute inset-x-0 top-0 z-20 h-5 bg-gradient-to-b from-background to-transparent"
          />
          {/* The field is anchored to the composer, not to the top bar: short
              content rests just above the composer (its children carry
              `min-h-full` + end-alignment), long content still scrolls from the
              top because it simply outgrows the container. */}
          <div
            ref={(node) => {
              taskScrollRef = node;
              // Start pinned: a conversation opens at its newest line.
              requestAnimationFrame(() => pin(true));
              // A markdown image has no intrinsic size until its bytes arrive,
              // so it lands as a late growth AFTER layout — the classic chat
              // scroll jump. `load` does not bubble, hence the capture phase.
              // Re-pinning only while following keeps a scrolled-up reader
              // exactly where they are.
              node.addEventListener(
                "load",
                () => {
                  if (following) pin(true);
                },
                true,
              );
            }}
            data-slot="task-scroll"
            onScroll={measure}
            /* `overflow-anchor` is what keeps a late-loading image or an
               expanding turn-details block from shoving the text the Owner is
               reading; the browser holds the anchored node still and absorbs
               the growth above it. */
            /* No `justify-end` here, ever: on a scroll container it makes a
               child taller than the viewport overflow UPWARD, and upward
               overflow is unreachable (scrollHeight never grows). Bottom
               anchoring for short content is the field's own job via
               `min-h-full` + its internal alignment. */
            class="flex min-h-0 flex-1 flex-col overflow-y-auto [overflow-anchor:auto]"
          >
            <Show
              when={focusedTask()}
              fallback={<AmbientField revealed={revealed()} loadingEarlier={loadingEarlier()} />}
            >
              {(task) => (
                <TaskField
                  task={task()}
                  tasks={tasks()}
                  views={taskViews()}
                  revealed={revealed()}
                  loadingEarlier={loadingEarlier()}
                />
              )}
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

          {/* The standard chat escape hatch, and the ONLY way the view returns
              to the bottom once the Owner has scrolled away. Quiet and
              self-effacing: it exists exactly while there is somewhere to jump
              to, sits in the field rather than over the composer, and says
              where it goes rather than shouting for attention. */}
          <Show when={canJump()}>
            <div class="pointer-events-none relative z-10 flex justify-center">
              <button
                type="button"
                data-slot="jump-to-latest"
                class="pointer-events-auto absolute bottom-1 inline-flex items-center gap-1.5 rounded-full border border-border/60 bg-surface px-3 py-1.5 text-xs text-surface-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/60 [@media(pointer:coarse)]:min-h-11"
                onClick={() => void jumpLatest(false)}
              >
                <ArrowDown class="size-3.5" aria-hidden="true" />
                Jump to latest
              </button>
            </div>
          </Show>

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
