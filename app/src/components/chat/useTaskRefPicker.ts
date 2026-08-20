import { createMemo, createSignal } from "solid-js";
import type { EventItem } from "../../protocol";
import { caretPoint, type CaretPoint } from "../../lib/caret";
import { createOverlayPresence } from "../../lib/focus";
import {
  detectRefQuery,
  filterTaskCandidates,
  insertTaskRef,
  type RefQuery,
} from "../../lib/task-ref";

/** The composer's Task-ref picker. Detection and insertion are pure (see
 * lib/task-ref.ts); this hook holds the reactive open/query/active-row state,
 * the caret anchor, and the keyboard controller the popup and its touch rows
 * share.
 *
 * It keeps no mention list of its own. The composed text is the truth, re-parsed
 * on send, so the only side effect an accepted row has is writing `#id ` at the
 * caret.
 *
 * While it is open it joins the overlay registry, which is what makes the whole
 * keyboard layer behave: the bare-key shortcuts stay quiet under it, and Esc
 * lands on the picker's rung of the ladder instead of clearing Task focus
 * behind it. */
export function createTaskRefPicker(opts: {
  getEl: () => HTMLTextAreaElement | undefined;
  value: () => string;
  setValue: (v: string) => void;
  tasks: () => EventItem[];
}) {
  const [open, setOpen] = createSignal(false);
  const [query, setQuery] = createSignal<RefQuery | null>(null);
  const [activeIndex, setActiveIndex] = createSignal(0);
  const [anchor, setAnchor] = createSignal<CaretPoint>({ x: 0, y: 0, lineHeight: 0 });

  const candidates = createMemo<EventItem[]>(() => {
    const q = query();
    if (!open() || q === null) return [];
    return filterTaskCandidates(opts.tasks(), q.query);
  });

  /** The active row, clamped to the live candidate list. -1 when empty. */
  const active = () => {
    const items = candidates();
    if (items.length === 0) return -1;
    return Math.min(activeIndex(), items.length - 1);
  };

  // Set by an explicit dismissal (Esc). A dismissed picker stays shut while the
  // caret wanders over the same half-typed token and comes back the moment the
  // Owner types again — an Esc that reopened on its own keyup would be no Esc
  // at all.
  let dismissed = false;

  function close(): void {
    setOpen(false);
    setQuery(null);
    setActiveIndex(0);
  }

  /** Re-evaluate after any text or caret change: open when the caret sits inside
   * a `#…` token AND that token can still name something, close otherwise. A
   * query that matches no Task closes rather than showing an empty box — an
   * empty popup over the composer is noise, and the typed text is still a
   * perfectly good ref if it turns out to name one. */
  function sync(typed = false): void {
    if (typed) dismissed = false;
    const el = opts.getEl();
    if (!el || dismissed) {
      close();
      return;
    }
    const caret = el.selectionStart ?? opts.value().length;
    const q = detectRefQuery(opts.value(), caret);
    if (q === null || filterTaskCandidates(opts.tasks(), q.query).length === 0) {
      close();
      return;
    }
    setQuery(q);
    setActiveIndex(0);
    // The popup is positioned in the capsule's row, not in the textarea, so the
    // field's own offset within that row is part of the anchor.
    const point = caretPoint(el, q.start);
    setAnchor({ ...point, x: point.x + el.offsetLeft });
    setOpen(true);
  }

  /** Write the chosen Task's ref at the caret, then close and restore the caret
   * after the token. */
  function accept(task: EventItem): void {
    const el = opts.getEl();
    const q = query();
    if (!el || !q) return;
    const caret = el.selectionStart ?? opts.value().length;
    const next = insertTaskRef(opts.value(), q, caret, task.id);
    opts.setValue(next.text);
    close();
    queueMicrotask(() => {
      el.focus();
      el.setSelectionRange(next.caret, next.caret);
    });
  }

  /** Composer keydown pre-handler. Returns true when the picker consumed the
   * event — only ever WHILE open, so the composer's own map (Enter send, Tab
   * queue, Esc stop) is untouched the rest of the time. */
  function handleKeyDown(e: KeyboardEvent): boolean {
    if (!open()) return false;
    const items = candidates();
    if (e.key === "Escape") {
      e.preventDefault();
      // Esc closes the picker and NOTHING else. The overlay presence below
      // already makes the global ladder yield, but the registry is released on
      // Solid's schedule while this listener runs synchronously, so the gesture
      // is also stopped here — one keystroke, one effect.
      e.stopPropagation();
      dismissed = true;
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

  createOverlayPresence(open);

  return {
    open,
    candidates,
    activeIndex: active,
    setActiveIndex,
    anchor,
    sync,
    accept,
    close,
    handleKeyDown,
  };
}
