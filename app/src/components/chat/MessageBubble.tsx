import { Check, Clock, Copy, RotateCcw, X } from "lucide-solid";
import { createSignal, Show } from "solid-js";
import type { DisplayMessage } from "../../store/types";
import { toast } from "../../lib/toast";
import { Markdown } from "../Markdown";
import { Bubble, BubbleContent } from "../ui/bubble";
import { Message, MessageContent, MessageFooter } from "../ui/message";
import { MessageAttachments } from "./MessageAttachments";
import { QuotedRef } from "./QuotedRef";

interface Props {
  message: DisplayMessage;
  refTarget: DisplayMessage | undefined;
  highlighted: boolean;
  queued: boolean;
  onTapQuote: (id: number) => void;
  onOpenImage: (src: string, alt: string) => void;
  onRetry: (clientId: string) => void;
  onCancelQueued: (clientId: string) => void;
}

const LONG_PRESS_MS = 450;

function formatTime(ts: string): string {
  try {
    return new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  } catch {
    return "";
  }
}

export function MessageBubble(props: Props) {
  const isOwner = () => props.message.author === "owner";
  const [copied, setCopied] = createSignal(false);
  let pressTimer: ReturnType<typeof setTimeout> | undefined;

  async function copyBody() {
    try {
      await navigator.clipboard.writeText(props.message.body);
      setCopied(true);
      setTimeout(() => setCopied(false), 1400);
      toast("Copied message");
    } catch {
      toast("Couldn't copy", { variant: "error" });
    }
  }

  function startPress() {
    pressTimer = setTimeout(() => void copyBody(), LONG_PRESS_MS);
  }
  function cancelPress() {
    if (pressTimer) clearTimeout(pressTimer);
    pressTimer = undefined;
  }

  return (
    <Message
      id={`msg-${props.message.id}`}
      align={isOwner() ? "end" : "start"}
      class="scroll-mt-4 px-3"
    >
      <MessageContent>
        <Bubble variant={isOwner() ? "default" : "muted"} align={isOwner() ? "end" : "start"}>
          <BubbleContent
            class="transition-shadow"
            classList={{ "ring-2 ring-ring": props.highlighted }}
            onPointerDown={startPress}
            onPointerUp={cancelPress}
            onPointerMove={cancelPress}
            onPointerCancel={cancelPress}
          >
            <Show when={props.message.ref !== null}>
              <QuotedRef
                message={props.refTarget}
                onTap={() => props.onTapQuote(props.message.ref as number)}
              />
            </Show>
            <Show when={props.message.body.length > 0}>
              <Markdown>{props.message.body}</Markdown>
            </Show>
            <Show when={props.message.attachments && props.message.attachments.length > 0}>
              <div classList={{ "mt-2": props.message.body.length > 0 }}>
                <MessageAttachments
                  attachments={props.message.attachments}
                  onOpenImage={props.onOpenImage}
                />
              </div>
            </Show>
          </BubbleContent>
        </Bubble>
        <MessageFooter class="gap-2 pt-0.5 text-[0.68rem]">
          {/* Queued (next_turn while a turn is active): cancellable chip. */}
          <Show when={props.queued && props.message.clientId}>
            <span class="inline-flex items-center gap-1 rounded-full bg-status-attention/15 px-1.5 py-px text-status-attention">
              <Clock class="size-3" aria-hidden="true" />
              queued
              <button
                type="button"
                class="ml-0.5 -mr-0.5 rounded-full p-px transition-colors hover:bg-status-attention/25"
                aria-label="Cancel queued message"
                onClick={() => props.onCancelQueued(props.message.clientId as string)}
              >
                <X class="size-3" />
              </button>
            </span>
          </Show>
          {/* Failed send: retry affordance. */}
          <Show when={props.message.failed && props.message.clientId}>
            <button
              type="button"
              class="inline-flex items-center gap-1 rounded-full bg-destructive/15 px-1.5 py-px text-destructive transition-colors hover:bg-destructive/25"
              onClick={() => props.onRetry(props.message.clientId as string)}
            >
              <RotateCcw class="size-3" aria-hidden="true" />
              Failed · retry
            </button>
          </Show>
          {/* Plain in-flight send. */}
          <Show when={props.message.pending && !props.message.failed && !props.queued}>
            <span class="italic">sending…</span>
          </Show>
          <span>{formatTime(props.message.ts)}</span>
          {/* Copy action: subtle, revealed on hover (desktop); long-press on touch. */}
          <button
            type="button"
            class="ml-auto rounded p-0.5 text-muted-foreground opacity-0 transition-opacity hover:text-foreground focus-visible:opacity-100 group-hover/message:opacity-100"
            aria-label="Copy message"
            onClick={() => void copyBody()}
          >
            <Show when={copied()} fallback={<Copy class="size-3.5" />}>
              <Check class="size-3.5 text-status-success" />
            </Show>
          </button>
        </MessageFooter>
      </MessageContent>
    </Message>
  );
}
