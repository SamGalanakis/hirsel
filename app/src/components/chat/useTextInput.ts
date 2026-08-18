import { createEffect, createSignal, onCleanup, onMount } from "solid-js";

const DRAFT_KEY_PREFIX = "hirsel.draft.";

function readDraft(key: string): string {
  try {
    return localStorage.getItem(DRAFT_KEY_PREFIX + key) ?? "";
  } catch {
    return "";
  }
}

function writeDraft(key: string, value: string): void {
  try {
    if (value.length > 0) localStorage.setItem(DRAFT_KEY_PREFIX + key, value);
    else localStorage.removeItem(DRAFT_KEY_PREFIX + key);
  } catch {
    // localStorage unavailable (private mode) — drafts just don't persist,
    // which is harmless: the composer still works, it only forgets on reload.
  }
}

/** Shared text-input mechanics for the standing Composer and constrained compact
 * input surfaces: a value signal, coarse-pointer
 * detection (fine pointers send on Enter, touch uses the send button), and an
 * auto-growing textarea bound via `setRef`. The keyboard map itself stays in
 * each caller (they layer Tab/Esc/ArrowUp differently), but this removes the
 * duplicated matchMedia probe, auto-grow effect, and focus/caret plumbing.
 *
 * Pass `persistKey` to keep the draft in localStorage keyed by surface (`main`,
 * identity): the draft is restored on mount and re-saved on
 * every keystroke, so leaving and reopening a surface never loses typed text.
 * A successful send clears the draft (setValue("") removes the stored key). */
export function useTextInput(maxHeightPx: number, persistKey?: string) {
  // Restore any saved draft synchronously at signal creation, before the persist
  // effect below first runs — an effect seeded with "" would otherwise clear the
  // stored draft before we could read it back.
  const [value, setValue] = createSignal(persistKey ? readDraft(persistKey) : "");
  const [coarse, setCoarse] = createSignal(false);
  let el: HTMLTextAreaElement | undefined;

  onMount(() => {
    const mq = window.matchMedia("(pointer: coarse)");
    setCoarse(mq.matches);
    const onChange = (e: MediaQueryListEvent) => setCoarse(e.matches);
    mq.addEventListener?.("change", onChange);
    onCleanup(() => mq.removeEventListener?.("change", onChange));
  });

  // Auto-grow the textarea up to a cap whenever the draft changes.
  createEffect(() => {
    value();
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, maxHeightPx)}px`;
  });

  // Persist the draft per surface. Runs immediately with the restored value
  // (a harmless write-back), then on every subsequent keystroke.
  if (persistKey) {
    createEffect(() => {
      writeDraft(persistKey, value());
    });
  }

  const setRef = (node: HTMLTextAreaElement) => {
    el = node;
  };
  const focus = () => el?.focus();
  const caretToEnd = () => {
    const node = el;
    if (!node) return;
    // Read length after the DOM reflects the just-set value.
    queueMicrotask(() => node.setSelectionRange(node.value.length, node.value.length));
  };

  // Land the caret at the end of a restored draft once the textarea exists, so
  // reopening a surface drops the Owner back where they left off.
  onMount(() => {
    if (persistKey && value().length > 0) caretToEnd();
  });

  return { value, setValue, coarse, setRef, focus, caretToEnd };
}
