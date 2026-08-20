import { Check, LoaderCircle } from "lucide-solid";
import { createMemo, createSignal, For, Show } from "solid-js";
import type { EventItem, ViewInstance } from "../../protocol";
import type { DisplayMessage } from "../../store/types";
import { decideEventWithUndo, reopenEvent } from "../../lib/event-decide";
import { PluginSlot } from "../../plugins/PluginSlot";
import { eventUiNodes, isEventResolved } from "../../store/selectors";
import { state } from "../../store/store";
import { getClient } from "../../ws/client";
import { EventCardRenderer } from "../../views/EventCardRenderer";
import { ViewRenderer } from "../../views/ViewRenderer";
import { Markdown } from "../Markdown";
import { Timeline, TurnDetails } from "../chat/Timeline";
import { splitStreamingReply } from "../chat/timeline";
import { CommittedToolCalls } from "../chat/ToolCalls";
import { messagesForTask, taskName } from "./task-model";
import { formatBytes } from "../../lib/format";

/** How much conversation the margin renders before asking to be widened, and
 * how much each "earlier messages" press reveals. The old hard `slice(-8)` had
 * no way to reach anything older at all; this window is generous enough that
 * the control is rare, and the reducer's MESSAGES_CAP is still the outer bound
 * (messages are KB-scale, so the whole retained log renders fine — there is no
 * virtualisation to earn here). */
const MARGIN_WINDOW = 30;

function ConversationMargin(props: { messages: DisplayMessage[]; thinking?: boolean }) {
  const [revealed, setRevealed] = createSignal(MARGIN_WINDOW);
  const shown = () => props.messages.slice(-revealed());
  const hiddenCount = () => Math.max(0, props.messages.length - revealed());
  const hasContent = () => props.messages.length > 0 || props.thinking || state.turnEvents.length > 0;
  return (
    <Show when={hasContent()}>
      <div data-slot="conversation-margin" class="min-w-0 py-4 rail:py-10">
        <div class="flex max-w-[42rem] flex-col gap-6">
          {/* Reaching older conversation is a deliberate press, not an infinite
              scroll: revealing more grows the flow ABOVE the reader, and the
              scroll container's anchoring keeps their place. */}
          <Show when={hiddenCount() > 0}>
            <button
              type="button"
              data-slot="reveal-earlier"
              class="-ml-1 w-fit rounded px-1 py-px text-xs text-muted-foreground underline decoration-current/30 underline-offset-4 transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/60"
              onClick={() => setRevealed((n) => n + MARGIN_WINDOW)}
            >
              {`Earlier messages (${hiddenCount()})`}
            </button>
          </Show>
          <Show when={props.messages.length > 0}>
            <For each={shown()}>
              {(message) => (
                <article
                  aria-label={message.author === "owner" ? "You" : "Hirsel"}
                  class="max-w-[42rem]"
                  classList={{
                    "ml-4 border-l border-border/50 pl-4 text-foreground":
                      message.author === "owner",
                    "pr-4 text-muted-foreground": message.author === "agent",
                  }}
                >
                  <Markdown>{message.body}</Markdown>
                  <Show when={message.attachments && message.attachments.length > 0}>
                    {/* Sent attachments stay quiet meta (DESIGN §3): a name and
                        its weight, which is what distinguishes a screenshot
                        from a pasted ref at a glance. Deliberately no preview —
                        rendering the bytes needs a get_blob_url round-trip per
                        message, which is a fetch path, not a layout tweak. */}
                    <ul class="mt-2 flex flex-wrap gap-x-3 font-mono text-xs text-muted-foreground">
                      <For each={message.attachments}>
                        {(item) => (
                          <li>
                            {item.name}
                            <span class="text-muted-foreground/60"> {formatBytes(item.size)}</span>
                          </li>
                        )}
                      </For>
                    </ul>
                  </Show>
                  <Show
                    when={state.turnDetails[message.id]?.length > 0}
                    fallback={
                      <Show when={message.tool_calls && message.tool_calls.length > 0}>
                        <div class="mt-2"><CommittedToolCalls toolCalls={message.tool_calls ?? []} /></div>
                      </Show>
                    }
                  >
                    <div class="mt-2"><TurnDetails events={state.turnDetails[message.id] ?? []} /></div>
                  </Show>
                </article>
              )}
            </For>
          </Show>
          <Show when={props.thinking || state.turnEvents.length > 0}>
            {(() => {
              const split = createMemo(() => splitStreamingReply(state.turnEvents));
              return (
                <div class="max-w-[42rem] text-muted-foreground">
                  {/* The thinking marker is suppressed once a reply is actually
                      streaming: the arriving text IS the liveness signal, and a
                      spinner above it just competes with the words. */}
                  <Show when={props.thinking && !split().reply}>
                    <div class="mb-3 flex items-center gap-2 text-sm">
                      <LoaderCircle class="size-3.5 motion-safe:animate-spin" aria-hidden="true" />
                      {state.agentActivity.text ?? "Thinking…"}
                    </div>
                  </Show>
                  <Show when={split().activity.length > 0}>
                    <Timeline events={split().activity} />
                  </Show>
                  {/* The reply being written, in the same typography as the
                      committed agent row it becomes — so the commit swaps the
                      draft out with no visible change. The markdown pipeline
                      stream-heals partial input, so a half-written fence or link
                      renders safely as it grows. */}
                  <Show when={split().reply}>
                    <article
                      aria-label="Hirsel"
                      data-slot="streaming-reply"
                      aria-busy="true"
                      class="max-w-[42rem] pr-4 text-muted-foreground"
                      classList={{ "mt-3": split().activity.length > 0 }}
                    >
                      <Markdown>{split().reply}</Markdown>
                    </article>
                  </Show>
                </div>
              );
            })()}
          </Show>
        </div>
      </div>
    </Show>
  );
}

export function AmbientField() {
  // The whole retained log: ConversationMargin owns the render window and the
  // "earlier messages" reveal, so the ambient field no longer truncates
  // history a second time on the way in.
  const messages = () => state.messages;
  return (
    <div
      data-slot="ambient-field"
      class="mx-auto flex min-h-full w-full max-w-frame flex-col justify-end px-gutter py-8 rail:py-12"
    >
      <div class="w-full max-w-measure">
        <ConversationMargin messages={messages()} thinking={state.agentActivity.state === "thinking"} />
        {/* home.section: plugin cards on the ambient field, below the recent
            global conversation — the resting view an Owner lands on with no Task
            focused, so an ambient plugin surface (a feed, a status card) belongs
            here and nowhere else. `ctx` is `{}`: there is no subject. */}
        <div class="mt-8 flex flex-col gap-4 empty:hidden">
          <PluginSlot name="home.section" />
        </div>
      </div>
    </div>
  );
}

export function TaskField(props: { task: EventItem; tasks: EventItem[]; views: ViewInstance[] }) {
  const related = () => messagesForTask(props.task, state.messages, props.tasks);
  const resolved = () => isEventResolved(props.task, state.eventDecideOverrides);
  // The generated instrument owns the Task's framing whenever it renders
  // anything at all — the same precedence `eventTitle()` applies (heading, else
  // status/text label, else the wire description). Gating on a heading alone
  // printed the description a second time under an instrument that already
  // stated it.
  const uiOwnsFraming = () => eventUiNodes(props.task.ui).length > 0;
  const hasConversation = () => related().length > 0
    || state.agentActivity.state === "thinking"
    || state.turnEvents.length > 0;
  return (
    <div
      data-slot="task-field"
      data-task={props.task.id}
      class="mx-auto grid min-h-full w-full max-w-frame content-end gap-14 px-gutter py-12 motion-safe:animate-in motion-safe:fade-in motion-safe:slide-in-from-left-2 motion-safe:duration-200"
      classList={{ "rail:grid-cols-[minmax(320px,1fr)_minmax(280px,.72fr)] rail:items-center": hasConversation() }}
    >
      <section class="min-w-0 max-w-measure">
        {/* Quiet identity: the task name is context, and the generated question
            below must visibly lead it (DESIGN §3). */}
        <h2 class="m-0 text-[1.25rem] font-[450] leading-tight text-muted-foreground">
          {taskName(props.task)}
        </h2>
        <Show when={!uiOwnsFraming()}>
          <p class="mt-2 max-w-[46ch] text-sm leading-relaxed text-muted-foreground">{props.task.description}</p>
        </Show>
        <div class="mt-4">
          <EventCardRenderer
            ui={props.task.ui}
            disabled={resolved()}
            onAction={(action, data, settles) => {
              if (settles) decideEventWithUndo(props.task.id, action, data);
              else getClient()?.sendEventAction(props.task.id, action, data);
            }}
          />
          <Show when={resolved()}>
            <div class="mt-6 flex items-center gap-3 text-sm text-status-success">
              <Check class="size-4" aria-hidden="true" />
              <span>Task decided</span>
              <button
                type="button"
                class="ml-1 inline-flex items-center rounded px-1 text-xs font-medium text-muted-foreground underline decoration-current/30 underline-offset-4 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/60 [@media(pointer:coarse)]:min-h-11 [@media(pointer:coarse)]:px-3"
                onClick={() => reopenEvent(props.task.id)}
              >
                Reopen
              </button>
            </div>
          </Show>
          <For each={props.views}>
            {(view) => (
              <div class="mt-6"><ViewRenderer spec={view.spec} instanceId={view.instance_id} placement={view.placement} /></div>
            )}
          </For>
        </div>
      </section>
      {/* The Task's margin column: its conversation, then task.panel plugin
          contributions. The margin is the Task world's secondary surface —
          context beside the instrument, never over it — which is exactly what a
          plugin panel about this Task is. `ctx` carries the Task's id. */}
      <div class="min-w-0">
        <ConversationMargin messages={related()} thinking={state.agentActivity.state === "thinking"} />
        <div class="flex flex-col gap-4 py-4 empty:hidden">
          <PluginSlot name="task.panel" ctx={{ taskId: props.task.id }} />
        </div>
      </div>
    </div>
  );
}
