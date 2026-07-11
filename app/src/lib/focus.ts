// Cross-surface focus handoff between the two composers that can be on screen
// at once (main Chat + an open Side Chat). Exactly one composer should hold
// focus at a time (design: "visible focus cues, one composer focused"), so
// leaving/closing a side chat returns focus to the main composer where the
// Owner's next keystroke would go. DOM-query based (rather than a passed ref)
// so it works across the component boundary without threading a ref through
// ChatView → SideChatSheet. Deferred a microtask so it runs after the sheet's
// own teardown has settled.

import { onCleanup } from "solid-js";

export function focusMainComposer(): void {
  queueMicrotask(() => {
    document.querySelector<HTMLTextAreaElement>('[data-composer="main"]')?.focus();
  });
}

const FOCUSABLE_SELECTOR = [
  "a[href]",
  "button:not([disabled])",
  "textarea:not([disabled])",
  'input:not([disabled]):not([type="hidden"])',
  "select:not([disabled])",
  '[tabindex]:not([tabindex="-1"])',
].join(",");

function focusablesWithin(panel: HTMLElement): HTMLElement[] {
  return Array.from(panel.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
    // Skip hidden nodes; `offsetParent === null` catches display:none ancestors.
    // The active element is kept even if the check is fooled (e.g. jsdom, where
    // layout is not computed), so tabbing never dead-ends.
    (el) => el.offsetParent !== null || el === document.activeElement,
  );
}

// A module-level stack so nested overlays compose: only the topmost live trap
// handles Tab/Escape and owns Tab-trapping, so opening a confirm dialog over a
// sheet transfers control to the dialog and hands it back on close, without the
// two traps fighting over focus.
const trapStack: symbol[] = [];

export interface FocusTrapOptions {
  /** Called when Escape is pressed while this trap is topmost (folds in the
   * per-overlay hand-rolled Escape listeners). */
  onEscape?: () => void;
  /** Gate Tab-trapping: return false to move initial focus + handle Escape +
   * restore focus, but leave Tab free (e.g. a desktop in-flow side rail beside a
   * still-live main chat, where trapping Tab would strand the keyboard). Omitted
   * = always trap (a true modal). Re-evaluated on each Tab. */
  trapTab?: () => boolean;
  /** Element to restore focus to on cleanup; defaults to whatever was focused
   * when the trap was created (the trigger). */
  restoreTo?: () => HTMLElement | null | undefined;
}

/** Focus management for an overlay panel: move focus into it on mount, trap Tab
 * within it (unless `trapTab` opts out), route Escape to `onEscape`, and restore
 * focus to the trigger on cleanup. Call inside a component's reactive scope
 * (onMount / a createEffect); it registers its own `onCleanup`. `getPanel`
 * returns the panel root element once it exists (give the root `tabindex={-1}`
 * so it can hold focus when it has no focusable children yet). */
export function createFocusTrap(
  getPanel: () => HTMLElement | undefined | null,
  options: FocusTrapOptions = {},
): void {
  const id = Symbol("focus-trap");
  const restoreEl = (options.restoreTo?.() ?? (document.activeElement as HTMLElement | null)) ?? null;
  trapStack.push(id);

  const isTopmost = () => trapStack[trapStack.length - 1] === id;
  const shouldTrapTab = () => options.trapTab?.() ?? true;

  // Move initial focus into the panel, deferred a microtask so the panel's own
  // mount (and any autofocus below it) has settled first.
  queueMicrotask(() => {
    if (!isTopmost()) return;
    const panel = getPanel();
    if (!panel || panel.contains(document.activeElement)) return;
    (focusablesWithin(panel)[0] ?? panel).focus();
  });

  const onKeyDown = (e: KeyboardEvent) => {
    if (!isTopmost()) return;
    if (e.key === "Escape") {
      options.onEscape?.();
      return;
    }
    if (e.key !== "Tab" || !shouldTrapTab()) return;
    const panel = getPanel();
    if (!panel) return;
    const items = focusablesWithin(panel);
    const active = document.activeElement as HTMLElement | null;
    if (items.length === 0) {
      e.preventDefault();
      panel.focus();
      return;
    }
    const first = items[0];
    const last = items[items.length - 1];
    if (e.shiftKey) {
      if (active === first || !panel.contains(active)) {
        e.preventDefault();
        last.focus();
      }
    } else if (active === last || !panel.contains(active)) {
      e.preventDefault();
      first.focus();
    }
  };

  window.addEventListener("keydown", onKeyDown);

  onCleanup(() => {
    window.removeEventListener("keydown", onKeyDown);
    const i = trapStack.lastIndexOf(id);
    if (i !== -1) trapStack.splice(i, 1);
    // Restore focus to the trigger, deferred so it lands after the panel has
    // unmounted (otherwise focus would bounce back into the disappearing node).
    queueMicrotask(() => restoreEl?.focus?.());
  });
}
