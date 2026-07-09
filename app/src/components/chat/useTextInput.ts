import { createEffect, createSignal, onCleanup, onMount } from "solid-js";

/** Shared text-input mechanics for the main Composer and the Inbox inline
 * ReplyInput: a value signal, coarse-pointer detection (fine pointers send on
 * Enter, touch uses the send button), and an auto-growing textarea bound via
 * `setRef`. The keyboard map itself stays in each caller because they differ —
 * the Composer layers Tab/Esc/ArrowUp on top — but this removes the duplicated
 * matchMedia probe, auto-grow effect, and focus/caret plumbing. */
export function useTextInput(maxHeightPx: number) {
  const [value, setValue] = createSignal("");
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

  return { value, setValue, coarse, setRef, focus, caretToEnd, el: () => el };
}
