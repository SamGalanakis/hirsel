// The composer submit keymap, shared by standing and compact input surfaces so
// submit behavior never drifts apart.
// Each caller layers its own surface-specific keys (the @-mention picker, Esc
// stop/cancel, Tab-to-queue) around this call — those differ per surface — but
// the common core (Cmd/Ctrl+Enter send, the coarse-pointer guard, Enter-send,
// and the ArrowUp recall of the last owner message) lives here in one place.

export interface SubmitKeymapHandlers {
  /** Current input text — used for the empty-input ArrowUp recall guard. */
  value: () => string;
  /** True on touch / coarse-pointer devices, where Enter inserts a newline and
   * the send button submits instead. */
  coarse: () => boolean;
  /** Submit the current draft. The surface picks the concrete mode (the main
   * Composer sends; a Cmd/Ctrl+Enter is always a plain send). */
  onSend: () => void;
  /** ArrowUp on an empty input recalls the last owner message. Return the text
   * to recall, or null when there is nothing to recall. Omit to disable recall
   * entirely when a compact input has no history to walk. */
  recallLast?: () => string | null;
  /** Apply a recalled draft: set the value and move the caret to the end. */
  onRecall?: (text: string) => void;
}

/** Handle the shared composer submit keys:
 *   • Cmd/Ctrl+Enter always sends, on every device.
 *   • ArrowUp on an empty input recalls the last owner message (when enabled).
 *   • On fine pointers, Enter sends and Shift+Enter is a newline; coarse pointers
 *     keep Enter as a newline and submit via the send button.
 * Returns true when it consumed the event, so the caller can early-return; when
 * it returns false the caller's own keys (Tab-to-queue, etc.) still run. */
export function handleSubmitKeys(e: KeyboardEvent, h: SubmitKeymapHandlers): boolean {
  // Cmd/Ctrl+Enter always sends, regardless of pointer type.
  if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
    e.preventDefault();
    h.onSend();
    return true;
  }
  // ArrowUp on an empty input recalls the last owner message.
  if (e.key === "ArrowUp" && h.recallLast && h.value().length === 0) {
    const last = h.recallLast();
    if (last !== null) {
      e.preventDefault();
      h.onRecall?.(last);
    }
    return true;
  }
  // Coarse pointers keep Enter as a newline; the send button submits.
  if (h.coarse()) return false;
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    h.onSend();
    return true;
  }
  return false;
}
