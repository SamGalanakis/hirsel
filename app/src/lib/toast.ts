import { createSignal } from "solid-js";

// Minimal dependency-free toast queue. A single module-level signal holds the
// active toasts; <Toaster/> renders them and `toast()` enqueues one. Kept tiny
// on purpose — no external toast lib, no bundle cost.

export type ToastVariant = "default" | "error";

export interface Toast {
  id: number;
  message: string;
  variant: ToastVariant;
}

const [toasts, setToasts] = createSignal<Toast[]>([]);
let nextId = 1;

const DEFAULT_MS = 4000;

export function toast(message: string, opts?: { variant?: ToastVariant; durationMs?: number }): void {
  const id = nextId++;
  setToasts((list) => [...list, { id, message, variant: opts?.variant ?? "default" }]);
  const ms = opts?.durationMs ?? DEFAULT_MS;
  setTimeout(() => dismissToast(id), ms);
}

export function dismissToast(id: number): void {
  setToasts((list) => list.filter((t) => t.id !== id));
}

export { toasts };
