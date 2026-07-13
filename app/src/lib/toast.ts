import { createSignal } from "solid-js";

// Minimal dependency-free toast queue. A single module-level signal holds the
// active toasts; <Toaster/> renders them and `toast()` enqueues one. Kept tiny
// on purpose — no external toast lib, no bundle cost.

export type ToastVariant = "default" | "error";

/** An optional inline action on a toast (e.g. "Undo" on a Marked-done toast).
 * Rendered as a button beside the message; tapping it runs `onClick` — the
 * caller is responsible for dismissing the toast if the action supersedes it. */
export interface ToastAction {
  label: string;
  onClick: () => void;
}

export interface Toast {
  id: number;
  message: string;
  variant: ToastVariant;
  action?: ToastAction;
}

const [toasts, setToasts] = createSignal<Toast[]>([]);
let nextId = 1;

const DEFAULT_MS = 4000;

/** Per-toast auto-dismiss timer state, so the countdown can PAUSE while the
 * toast is hovered/keyboard-focused (spec item 7) — a user reaching for "Undo"
 * must not lose it — and RESUME with the time it had left on leave. */
interface Countdown {
  handle: ReturnType<typeof setTimeout> | null;
  /** Milliseconds still owed when the current run started. */
  remaining: number;
  /** When the current run started, so a pause can debit the elapsed slice. */
  startedAt: number;
}
const countdowns = new Map<number, Countdown>();

function run(id: number, ms: number): void {
  const cd: Countdown = {
    handle: setTimeout(() => dismissToast(id), ms),
    remaining: ms,
    startedAt: Date.now(),
  };
  countdowns.set(id, cd);
}

/** Enqueue a toast; returns its id so a caller can dismiss it early (e.g. when
 * its action is taken before the timeout). */
export function toast(
  message: string,
  opts?: { variant?: ToastVariant; durationMs?: number; action?: ToastAction },
): number {
  const id = nextId++;
  setToasts((list) => [
    ...list,
    { id, message, variant: opts?.variant ?? "default", action: opts?.action },
  ]);
  run(id, opts?.durationMs ?? DEFAULT_MS);
  return id;
}

/** Pause a toast's auto-dismiss (hover/focus enter). Debits the elapsed slice
 * from what's left and clears the pending timer; idempotent. */
export function pauseToast(id: number): void {
  const cd = countdowns.get(id);
  if (!cd || cd.handle === null) return;
  clearTimeout(cd.handle);
  cd.remaining = Math.max(0, cd.remaining - (Date.now() - cd.startedAt));
  cd.handle = null;
}

/** Resume a paused toast's auto-dismiss (hover/focus leave) with the time it
 * had left; idempotent (a no-op if already running or already gone). */
export function resumeToast(id: number): void {
  const cd = countdowns.get(id);
  if (!cd || cd.handle !== null) return;
  cd.startedAt = Date.now();
  cd.handle = setTimeout(() => dismissToast(id), cd.remaining);
}

export function dismissToast(id: number): void {
  const cd = countdowns.get(id);
  if (cd?.handle) clearTimeout(cd.handle);
  countdowns.delete(id);
  setToasts((list) => list.filter((t) => t.id !== id));
}

export { toasts };
