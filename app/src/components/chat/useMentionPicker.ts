import { createMemo, createSignal } from "solid-js";
import type { Ping } from "../../protocol";
import {
  detectMentionQuery,
  filterMentionCandidates,
  insertMention,
  type MentionQuery,
} from "./mentions";

/** Composer @-mention picker (v2.1). Detection/insertion is pure (see
 * mentions.ts); this hook holds the reactive open/query/active-row state and
 * the keyboard controller shared by the desktop popup and the phone chip row.
 *
 * It deliberately does NOT track a mention list of its own: the composed text
 * is the source of truth, re-parsed on send (resolveMentionIds). The picker's
 * only side effect is inserting the `@name ` token at the caret. */
export function useMentionPicker(opts: {
  getEl: () => HTMLTextAreaElement | undefined;
  value: () => string;
  setValue: (v: string) => void;
}) {
  const [open, setOpen] = createSignal(false);
  const [query, setQuery] = createSignal<MentionQuery | null>(null);
  const [activeIndex, setActiveIndex] = createSignal(0);

  // Mentions are sourceless until the event-seeded rewire (Ping slice retired, Wave 1); the picker never opens meanwhile.
  const candidates = createMemo<Ping[]>(() => {
    const q = query();
    if (!open() || q === null) return [];
    return filterMentionCandidates([], q.query);
  });

  /** Clamp the active row to the current candidate list. */
  const active = () => {
    const items = candidates();
    if (items.length === 0) return -1;
    return Math.min(activeIndex(), items.length - 1);
  };

  function close(): void {
    setOpen(false);
    setQuery(null);
    setActiveIndex(0);
  }

  /** Re-evaluate the picker after any text or caret change. Opens when the
   * caret is inside a `@handle` being typed and at least one open Ping exists;
   * closes otherwise. */
  function sync(): void {
    const el = opts.getEl();
    if (!el) {
      close();
      return;
    }
    const caret = el.selectionStart ?? opts.value().length;
    const q = detectMentionQuery(opts.value(), caret);
    if (q === null) {
      close();
      return;
    }
    setQuery(q);
    setActiveIndex(0);
    setOpen(true);
  }

  /** Insert the chosen Ping's `@name` at the caret, then close and restore the
   * caret after the inserted token. */
  function accept(ping: Ping): void {
    const el = opts.getEl();
    const q = query();
    if (!el || !q) return;
    const caret = el.selectionStart ?? opts.value().length;
    const { text, caret: nextCaret } = insertMention(opts.value(), q, caret, ping.name);
    opts.setValue(text);
    close();
    queueMicrotask(() => {
      el.focus();
      el.setSelectionRange(nextCaret, nextCaret);
    });
  }

  /** Composer keydown pre-handler. Returns true when the picker consumed the
   * event (Up/Down/Enter/Tab/Esc) — WHILE open — so the composer's own keymap
   * (Enter=send, Tab=queue, Esc=cancel) is untouched when the picker is closed. */
  function handleKeyDown(e: KeyboardEvent): boolean {
    if (!open()) return false;
    const items = candidates();
    if (e.key === "Escape") {
      e.preventDefault();
      close();
      return true;
    }
    if (items.length === 0) return false;
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        setActiveIndex((i) => (Math.min(i, items.length - 1) + 1) % items.length);
        return true;
      case "ArrowUp":
        e.preventDefault();
        setActiveIndex((i) => (Math.min(i, items.length - 1) - 1 + items.length) % items.length);
        return true;
      case "Enter":
      case "Tab":
        e.preventDefault();
        accept(items[active()]);
        return true;
      default:
        return false;
    }
  }

  return {
    open,
    candidates,
    activeIndex: active,
    setActiveIndex,
    query,
    sync,
    accept,
    close,
    handleKeyDown,
  };
}
