import { ArrowUp, X } from "lucide-solid";
import { createEffect, createSignal, Show } from "solid-js";
import type { DisplayMessage } from "../../store/types";
import { Button } from "../ui/button";
import { Textarea } from "../ui/textarea";

const MAX_HEIGHT_PX = 112;

interface Props {
  replyingTo: DisplayMessage | undefined | null;
  onCancelReply: () => void;
  onSend: (body: string, ref: number | null) => void;
}

function snippet(body: string): string {
  const oneLine = body.replace(/\s+/g, " ").trim();
  return oneLine.length > 60 ? `${oneLine.slice(0, 60)}…` : oneLine;
}

/** Composer pinned above the tab bar. Enter inserts a newline (mobile
 * default); Ctrl/Cmd+Enter sends (desktop convenience). */
export function Composer(props: Props) {
  const [value, setValue] = createSignal("");
  let textareaRef: HTMLTextAreaElement | undefined;

  // Auto-grow the textarea up to a cap whenever the draft changes.
  createEffect(() => {
    value(); // track
    const el = textareaRef;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, MAX_HEIGHT_PX)}px`;
  });

  // Focus the composer when a reply is pre-quoted into it.
  createEffect(() => {
    if (props.replyingTo) textareaRef?.focus();
  });

  function send() {
    const body = value().trim();
    if (body.length === 0) return;
    props.onSend(body, props.replyingTo?.id ?? null);
    setValue("");
    if (props.replyingTo) props.onCancelReply();
  }

  function handleKeyDown(e: KeyboardEvent) {
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      send();
    }
  }

  return (
    <div class="flex-shrink-0 border-t border-border bg-card px-3 py-2">
      <Show when={props.replyingTo}>
        {(replyingTo) => (
          <div class="mb-2 flex items-start gap-2 rounded-md border-l-2 border-primary bg-black/15 px-2 py-1">
            <div class="min-w-0 flex-1">
              <div class="text-[0.68rem] uppercase tracking-[0.03em] text-primary">
                Replying to {replyingTo().author === "owner" ? "you" : "Agent"}
              </div>
              <div class="overflow-hidden text-ellipsis whitespace-nowrap text-xs text-muted-foreground">
                {snippet(replyingTo().body)}
              </div>
            </div>
            <button
              type="button"
              class="p-0.5 text-muted-foreground transition-colors hover:text-foreground"
              onClick={() => props.onCancelReply()}
              aria-label="Cancel reply"
            >
              <X class="size-4" />
            </button>
          </div>
        )}
      </Show>
      <div class="flex items-end gap-2">
        <Textarea
          ref={textareaRef}
          rows={1}
          class="max-h-28 min-h-0 flex-1 resize-none py-2 leading-snug"
          placeholder="Message the Agent…"
          value={value()}
          onInput={(e) => setValue(e.currentTarget.value)}
          onKeyDown={handleKeyDown}
        />
        <Button
          type="button"
          size="icon"
          class="shrink-0 rounded-full"
          onClick={send}
          disabled={value().trim().length === 0}
          aria-label="Send"
        >
          <ArrowUp class="size-5" />
        </Button>
      </div>
    </div>
  );
}
