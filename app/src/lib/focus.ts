// Focus handoff for the standing Hirsel composer and the flat task index.
// DOM-query based so summoned utilities can return attention without threading
// refs through the shell. Deferred a microtask so it runs after teardown.

import { createSignal, onCleanup } from "solid-js";

/** A reactive boolean tracking a CSS media query, kept in sync via the
 * MediaQueryList `change` event. Lets a component read a breakpoint (e.g. "is
 * this sheet full-screen on phone right now?") as ordinary reactive state — so
 * `role`/`aria-modal` can be correct on phone (a true modal) yet absent on the
 * desktop in-flow pane. Call inside a reactive scope; it self-cleans. Guards
 * `matchMedia` for non-DOM/test environments (defaults to false). */
export function createMediaFlag(query: string): () => boolean {
  const mql = typeof window !== "undefined" && window.matchMedia
    ? window.matchMedia(query)
    : null;
  const [flag, setFlag] = createSignal(mql?.matches ?? false);
  if (mql) {
    const on = () => setFlag(mql.matches);
    mql.addEventListener?.("change", on);
    onCleanup(() => mql.removeEventListener?.("change", on));
  }
  return flag;
}

export function focusMainComposer(): void {
  queueMicrotask(() => {
    document.querySelector<HTMLTextAreaElement>('[data-composer="main"]')?.focus();
  });
}

/** Move focus to the open task, or the first task when the global field is
 * active. This is the task-world peer of `focusMainComposer`. */
export function focusTaskIndex(): void {
  queueMicrotask(() => {
    const nav = document.querySelector<HTMLElement>('[data-slot="task-index"]');
    const task = nav?.querySelector<HTMLElement>('[aria-current="page"]') ??
      nav?.querySelector<HTMLElement>("button");
    task?.focus();
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
    (el) => {
      if (el.closest('[hidden], [inert], [aria-hidden="true"]')) return false;
      const style = window.getComputedStyle(el);
      if (style.display === "none" || style.visibility === "hidden") return false;
      return Array.from(el.getClientRects()).some((rect) => rect.width > 0 && rect.height > 0);
    },
  );
}

interface LiveFocusTrap {
  id: symbol;
  getPanel: () => HTMLElement | undefined | null;
}

// A module-level stack so nested overlays compose: only the topmost live trap
// handles Tab/Escape and owns Tab-trapping, so opening a confirm dialog over a
// sheet transfers control to the dialog and hands it back on close, without the
// two traps fighting over focus. Keeping the panel getter also lets cleanup
// distinguish a nested-dialog handoff from a different utility taking over.
const trapStack: LiveFocusTrap[] = [];

/** Whether any modal overlay / focus trap is currently open. Callers yield Esc:
 * an Esc meant to dismiss an open overlay must not also trigger a background
 * action (e.g. the main composer stopping a live agent turn). Derived from the
 * trap stack so it stays accurate as overlays open and close. */
export function anyOverlayOpen(): boolean {
  return trapStack.length > 0;
}

export interface FocusTrapOptions {
  /** Called when Escape is pressed while this trap is topmost (folds in the
   * per-overlay hand-rolled Escape listeners). */
  onEscape?: () => void;
  /** Gate Tab-trapping: return false to move initial focus + handle Escape +
   * restore focus, but leave Tab free (e.g. a desktop in-flow side rail beside a
   * still-live standing conversation, where trapping Tab would strand the keyboard). Omitted
   * = always trap (a true modal). Re-evaluated on each Tab. */
  trapTab?: () => boolean;
  /** Preferred element to restore focus to on cleanup; re-evaluated after the
   * panel unmounts so a responsive trigger can become the correct destination
   * while the utility is open. Falls back to whatever was focused when the
   * trap was created. */
  restoreTo?: () => HTMLElement | null | undefined;
}

/** The stable trigger used when a phone utility sheet is dismissed. It remains
 * mounted while a utility is open, unlike the portaled menu item that launched
 * it. Kept here so every utility uses exactly the same restoration contract. */
export function phoneUtilityRestoreTarget(): HTMLElement | null {
  return document.querySelector<HTMLElement>('[data-slot="phone-overflow-trigger"]');
}

/** Processes has its own standing header control, so its inspector returns to
 * that control instead of the unrelated utility overflow. */
export function processesRestoreTarget(): HTMLElement | null {
  return document.querySelector<HTMLElement>('[data-slot="processes-trigger"]');
}

function canReceiveRestoredFocus(element: HTMLElement | null | undefined): element is HTMLElement {
  if (!element?.isConnected || element.hasAttribute("disabled")) return false;
  if (element.closest('[hidden], [inert], [aria-hidden="true"]')) return false;
  const style = window.getComputedStyle?.(element);
  return style?.display !== "none" && style?.visibility !== "hidden";
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
  const capturedRestoreEl = (document.activeElement as HTMLElement | null) ?? null;
  trapStack.push({ id, getPanel });

  const isTopmost = () => trapStack[trapStack.length - 1]?.id === id;
  const shouldTrapTab = () => options.trapTab?.() ?? true;

  // Move initial focus after the next paint. Overlay/menu libraries commonly
  // restore focus to their trigger during teardown; a microtask here races
  // that restoration and can leave a freshly-opened sheet focused behind
  // itself. One animation frame lets both the old overlay and responsive CSS
  // settle, so visibility is measured against the UI the user can actually see.
  let ensureFocusFrame: number | null = null;
  const ensureVisibleFocus = () => {
    if (!isTopmost()) return;
    const panel = getPanel();
    if (!panel) return;
    const active = document.activeElement as HTMLElement | null;
    const visibleItems = focusablesWithin(panel);
    if (active === panel || (active !== null && visibleItems.includes(active))) return;
    (visibleItems[0] ?? panel).focus();
  };
  const scheduleVisibleFocus = () => {
    if (ensureFocusFrame !== null) window.cancelAnimationFrame(ensureFocusFrame);
    ensureFocusFrame = window.requestAnimationFrame(() => {
      ensureFocusFrame = null;
      ensureVisibleFocus();
    });
  };
  queueMicrotask(scheduleVisibleFocus);
  // A desktop inspector can become a modal sheet without remounting. Its
  // desktop close button then has a zero box while still being active; repair
  // that handoff after the viewport settles.
  window.addEventListener("resize", scheduleVisibleFocus);

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
    const activeIndex = active === null ? -1 : items.indexOf(active);
    const next = e.shiftKey
      ? activeIndex <= 0 ? last : items[activeIndex - 1]
      : activeIndex === -1 || active === last ? first : items[activeIndex + 1];
    // A portaled modal's descendants are not guaranteed to be contiguous in
    // the document-wide tab order. Own every modal Tab step rather than only
    // wrapping the edges, so focus cannot visit background controls between
    // two controls that are both inside the panel.
    e.preventDefault();
    // Run in the window capture phase and stop this Tab completely. Portal and
    // overlay libraries may install their own bubbling focus restoration; if
    // they run after this handler they can move focus back outside the still
    // open modal. A true modal owns the entire Tab gesture.
    e.stopImmediatePropagation();
    next.focus();
  };

  const onFocusIn = (event: FocusEvent) => {
    if (!isTopmost() || !shouldTrapTab()) return;
    const panel = getPanel();
    const target = event.target;
    if (!panel || !(target instanceof HTMLElement)) return;
    if (target === panel || focusablesWithin(panel).includes(target)) return;
    // Overlay libraries may restore their trigger while a menu tears down.
    // Reclaim on the next paint, after that restoration stack has settled, to
    // avoid a synchronous focus tug-of-war while keeping the open modal owned.
    scheduleVisibleFocus();
  };

  // Live process rows can change representation as they finish. If the
  // focused row is replaced, browsers move focus to BODY without a useful
  // focusin target. Observe the modal subtree and repair that handoff after
  // Solid has completed the DOM mutation.
  const panelObserver = new MutationObserver(() => {
    if (!shouldTrapTab()) return;
    queueMicrotask(ensureVisibleFocus);
  });
  const observedPanel = getPanel();
  if (observedPanel) {
    panelObserver.observe(observedPanel, { childList: true, subtree: true });
  }

  window.addEventListener("keydown", onKeyDown, true);
  window.addEventListener("focusin", onFocusIn);

  onCleanup(() => {
    if (ensureFocusFrame !== null) window.cancelAnimationFrame(ensureFocusFrame);
    window.removeEventListener("resize", scheduleVisibleFocus);
    window.removeEventListener("keydown", onKeyDown, true);
    window.removeEventListener("focusin", onFocusIn);
    panelObserver.disconnect();
    const i = trapStack.findIndex((trap) => trap.id === id);
    if (i !== -1) trapStack.splice(i, 1);
    // Resolve the preferred target after teardown. This matters when a desktop
    // inspector becomes a phone sheet while it is open: the visible phone
    // overflow is now the honest destination even though it was not the launch
    // target. A newly-opened sibling utility retains focus; a nested overlay may
    // only hand focus back to an element inside the still-live parent panel.
    queueMicrotask(() => {
      const preferred = options.restoreTo?.();
      const target = canReceiveRestoredFocus(preferred)
        ? preferred
        : canReceiveRestoredFocus(capturedRestoreEl)
          ? capturedRestoreEl
          : null;
      const parentTrap = trapStack[trapStack.length - 1];
      if (parentTrap) {
        if (target && parentTrap.getPanel()?.contains(target)) target.focus();
        return;
      }
      target?.focus();
    });
  });
}
