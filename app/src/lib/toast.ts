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
  const ms = opts?.durationMs ?? DEFAULT_MS;
  setTimeout(() => dismissToast(id), ms);
  return id;
}

export function dismissToast(id: number): void {
  setToasts((list) => list.filter((t) => t.id !== id));
}

export { toasts };
