import { Show } from "solid-js";
import type { DisplayMessage } from "../../store/types";
import { Markdown } from "../Markdown";
import { Bubble, BubbleContent } from "../ui/bubble";
import { Message, MessageContent, MessageFooter } from "../ui/message";
import { QuotedRef } from "./QuotedRef";

interface Props {
  message: DisplayMessage;
  refTarget: DisplayMessage | undefined;
  highlighted: boolean;
  onTapQuote: (id: number) => void;
}

function formatTime(ts: string): string {
  try {
    return new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  } catch {
    return "";
  }
}

export function MessageBubble(props: Props) {
  const isOwner = () => props.message.author === "owner";
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
          >
            <Show when={props.message.ref !== null}>
              <QuotedRef
                message={props.refTarget}
                onTap={() => props.onTapQuote(props.message.ref as number)}
              />
            </Show>
            <Markdown>{props.message.body}</Markdown>
          </BubbleContent>
        </Bubble>
        <MessageFooter class="gap-2 pt-0.5 text-[0.68rem]">
          <Show when={props.message.pending}>
            <span class="italic">sending…</span>
          </Show>
          <span>{formatTime(props.message.ts)}</span>
        </MessageFooter>
      </MessageContent>
    </Message>
  );
}
