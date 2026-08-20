import { Check, LoaderCircle } from "lucide-solid";
import { createMemo, For, Show } from "solid-js";
import type { EventItem, ViewInstance } from "../../protocol";
import type { DisplayMessage } from "../../store/types";
import { copyWithToast } from "../../lib/clipboard";
import { decideEventWithUndo, reopenEvent } from "../../lib/event-decide";
import { formatTaskRef } from "../../lib/task-ref";
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

/** The one focus-swap motion, shared verbatim by both fields so the swap reads
 * as one surface changing subject rather than two panels trading places: a
 * single 200ms fade + 8px settle in the same direction, whichever way focus
 * moves. It is deliberately the ONLY thing that moves — the composer, the task
 * strip and the floating ⋯ are stationary through the swap — and it stays on
 * the left/up axes because a rightward or downward translate inside the
 * `overflow-y-auto` field would push a transient scrollbar. `motion-safe`
 * collapses the whole thing to an instant swap under prefers-reduced-motion. */
const FIELD_SWAP =
  "motion-safe:animate-in motion-safe:fade-in motion-safe:slide-in-from-left-2 motion-safe:duration-200";

/** One vertical rhythm for both fields. They alternate in the same slot and are
 * both bottom-anchored, so a different inset would drop the content by that
 * difference on every focus toggle. */
const FIELD_PADDING = "px-gutter py-8 rail:py-12";

/** The pinned task card's ceiling. A content-thin task ("session rotated", two
 * lines) reads as a few lines; a task with tall generated fields stops here and
 * scrolls inside itself rather than shoving the conversation off screen. It is
 * `dvh` rather than a fixed height because the budget being divided is the
 * viewport: on a 700px phone this leaves the conversation ~60dvh minus the
 * composer, which is still the majority of the field. */
const CARD_MAX_HEIGHT = "max-h-[40dvh]";

interface ConversationProps {
  messages: DisplayMessage[];
  revealed: number;
  loadingEarlier: boolean;
  thinking?: boolean;
}

function Conversation(props: ConversationProps) {
  const shown = () => props.messages.slice(-props.revealed);
  const hasContent = () => props.messages.length > 0 || props.thinking || state.turnEvents.length > 0;
  return (
    <Show when={hasContent()}>
      <div data-slot="conversation" class="min-w-0 py-4 rail:py-10">
        {/* No second cap here. The reading measure IS this column now
            (`--container-measure`), so a `max-w-[42rem]` inside it only made the
            prose narrower than the card and the composer it sits between — the
            overhang the Owner saw under the last line of text. */}
        <div class="flex flex-col gap-6">
          <Show when={props.loadingEarlier}>
            <div
              data-slot="loading-earlier"
              role="status"
              class="text-xs text-muted-foreground"
            >
              Loading earlier…
            </div>
          </Show>
          <Show when={props.messages.length > 0}>
            <For each={shown()}>
              {(message) => (
                <article
                  aria-label={message.author === "owner" ? "You" : "Hirsel"}
                  data-message-id={message.id}
                  class="min-w-0"
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
                <div class="min-w-0 text-muted-foreground">
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
                      class="min-w-0 pr-4 text-muted-foreground"
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

export function AmbientField(props: { revealed: number; loadingEarlier: boolean }) {
  // The whole retained log; TaskShell owns just-in-time window expansion because
  // it also owns the scroll geometry and Host backfill boundary.
  const messages = () => state.messages;
  return (
    <div
      data-slot="ambient-field"
      /* `shrink-0` is load-bearing: the scroller is a flex column, and a flex
         item's default shrink would pin this field at the container height
         while its content overflows out the top past `justify-end` — the
         conversation must GROW the field so the scroller has something to
         scroll. */
      class={`mx-auto flex min-h-full w-full max-w-frame shrink-0 flex-col justify-end ${FIELD_PADDING} ${FIELD_SWAP}`}
    >
      <div class="w-full max-w-measure">
        <Conversation
          messages={messages()}
          revealed={props.revealed}
          loadingEarlier={props.loadingEarlier}
          thinking={state.agentActivity.state === "thinking"}
        />
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

export function TaskField(props: {
  task: EventItem;
  tasks: EventItem[];
  views: ViewInstance[];
  revealed: number;
  loadingEarlier: boolean;
}) {
  const related = () => messagesForTask(props.task, state.messages, props.tasks);
  const resolved = () => isEventResolved(props.task);
  // The generated instrument owns the Task's framing whenever it renders
  // anything at all — the same precedence `eventTitle()` applies (heading, else
  // status/text label, else the wire description). Gating on a heading alone
  // printed the description a second time under an instrument that already
  // stated it.
  const uiOwnsFraming = () => eventUiNodes(props.task.ui).length > 0;
  return (
    <div
      data-slot="task-field"
      data-task={props.task.id}
      /* `min-h-full` is load-bearing, not decoration: it fills the scroll
         container so a content-thin task still owns the field's vertical
         rhythm (card at top, room below) instead of collapsing to a strip.
         The scroll container itself carries no alignment — see TaskShell. */
      class={`mx-auto flex min-h-full w-full max-w-frame shrink-0 flex-col ${FIELD_PADDING} ${FIELD_SWAP}`}
    >
      {/* ONE column at the reading measure, the same one ambient uses. The task
          is a pinned context card at its top and the conversation flows below
          it into the composer — allocation follows content, not role. (This was
          two columns: the instrument left, the conversation squeezed into a
          ~400px right margin. A two-line "session rotated" notice then owned
          half the screen while everything with substance sat in the margin.)

          Both halves are `grid-cols-[minmax(0,1fr)]`, never a bare implicit
          column: an auto track sizes to its item's min-content, so one wide
          table or unbroken command line — in the card OR in the conversation —
          would widen the whole field past the viewport instead of scrolling
          inside its own box. The column itself is flex rather than grid because
          a grid item's containing block is its own track, which would pin the
          sticky card to a zero-travel row and defeat the pin entirely. */}
      <div
        data-slot="task-column"
        class="flex w-full max-w-measure flex-1 flex-col"
      >
        <section
          data-slot="task-card"
          /* Pinned: the card stays at the top of the scroll container so the
             task stays legible while its history is read. It is on the canvas
             like everything else — `bg-background` is what the conversation
             passes behind, not a decorative surface — with one hairline as the
             boundary (DESIGN §2). Capped at CARD_MAX_HEIGHT with its own
             scroll so a tall instrument can never push the conversation off
             screen. Scroll chaining is deliberately left at its default: the
             pinned card covers the top of the field, so it is what most wheel
             gestures land on, and `overscroll-contain` would trap them there
             once the card reached its own end. */
          class={`sticky top-0 z-10 grid shrink-0 grid-cols-[minmax(0,1fr)] overflow-y-auto border-b border-border/50 bg-background pb-6 pt-2 ${CARD_MAX_HEIGHT}`}
        >
          <div class="min-w-0">
            {/* Quiet identity: the task name is context, and the generated question
                below must visibly lead it (DESIGN §3). The ref leads the name in
                the same order the chip uses, so the two readings of one Task
                line up. It is the only thing on the card that can be taken with
                you: one click copies `#12`, which is the exact string that
                cites this Task back in the composer. The deep link is the
                address bar's job — it already says /t/12 while this Task is
                open — so the card does not offer a second, longer copy of the
                same idea. */}
            <div class="flex items-baseline gap-2">
              <button
                type="button"
                data-slot="task-ref-copy"
                aria-label={`Copy task ref ${formatTaskRef(props.task.id)}`}
                title="Copy ref"
                class="shrink-0 rounded text-meta text-muted-foreground/70 outline-none transition-colors hover:text-primary focus-visible:ring-2 focus-visible:ring-ring/60 [@media(pointer:coarse)]:-mx-2 [@media(pointer:coarse)]:-my-3 [@media(pointer:coarse)]:min-h-11 [@media(pointer:coarse)]:min-w-11"
                onClick={() =>
                  void copyWithToast(
                    formatTaskRef(props.task.id),
                    `Copied ${formatTaskRef(props.task.id)}`,
                  )}
              >
                {/* Mono on the text, not the button: see TaskRefTag. */}
                <span class="font-mono">{formatTaskRef(props.task.id)}</span>
              </button>
              <h2 class="m-0 min-w-0 text-[1.25rem] font-[450] leading-tight text-muted-foreground">
                {taskName(props.task)}
              </h2>
            </div>
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
          </div>
        </section>
        {/* The Task's conversation, then task.panel plugin contributions —
            below the card, at the same width, bottom-anchored on the composer
            exactly as ambient conversation is. `ctx` carries the Task's id. */}
        <div data-slot="task-conversation" class="grid flex-1 grid-cols-[minmax(0,1fr)] content-end">
          <Conversation
            messages={related()}
            revealed={props.revealed}
            loadingEarlier={props.loadingEarlier}
            thinking={state.agentActivity.state === "thinking"}
          />
          <div class="flex flex-col gap-4 py-4 empty:hidden">
            <PluginSlot name="task.panel" ctx={{ taskId: props.task.id }} />
          </div>
        </div>
      </div>
    </div>
  );
}
