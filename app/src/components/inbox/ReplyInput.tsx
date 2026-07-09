import { ArrowUp } from "lucide-solid";
import { Button } from "../ui/button";
import { Textarea } from "../ui/textarea";
import { useTextInput } from "../chat/useTextInput";

const MAX_HEIGHT_PX = 96;

interface Props {
  /** Send the typed body anchored to the item (ref handled by the caller). */
  onSend: (body: string) => void;
  /** Focus on mount — used when a card reveals the input via "Reply". */
  autofocus?: boolean;
  placeholder?: string;
}

/** Compact single-line reply input for an Inbox card — a mini composer that
 * grows a little as you type. Shares the auto-grow + coarse-pointer mechanics
 * with the main Composer (useTextInput); Enter sends on fine pointers
 * (Shift+Enter newline), the send button submits on touch. It does not own the
 * send path: it just hands the body up, and the card routes it through the same
 * `getClient().sendMessage(body, item.anchor)` an owner Composer send uses. */
export function ReplyInput(props: Props) {
  const input = useTextInput(MAX_HEIGHT_PX);

  function submit() {
    const body = input.value().trim();
    if (body.length === 0) return;
    props.onSend(body);
    input.setValue("");
    input.focus();
  }

  function handleKeyDown(e: KeyboardEvent) {
    // Cmd/Ctrl+Enter always sends, on every device.
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      submit();
      return;
    }
    if (input.coarse()) return; // touch: Enter is a newline; the button sends.
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      submit();
    }
  }

  const canSend = () => input.value().trim().length > 0;

  return (
    <div class="mt-2 flex items-end gap-2">
      <Textarea
        ref={(node) => {
          input.setRef(node);
          if (props.autofocus) queueMicrotask(() => node.focus());
        }}
        rows={1}
        class="max-h-24 min-h-0 flex-1 resize-none rounded-md border border-border bg-background px-2.5 py-1.5 text-sm leading-snug"
        placeholder={props.placeholder ?? "Reply…"}
        value={input.value()}
        onInput={(e) => input.setValue(e.currentTarget.value)}
        onKeyDown={handleKeyDown}
      />
      <Button
        type="button"
        size="icon"
        class="size-8 shrink-0 rounded-full"
        onClick={submit}
        disabled={!canSend()}
        aria-label="Send reply"
      >
        <ArrowUp class="size-4" />
      </Button>
    </div>
  );
}
