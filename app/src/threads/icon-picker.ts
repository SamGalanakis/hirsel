import { createSignal } from "solid-js";
import { historyId } from "../lib/history";
import type { Thread } from "./types";

export const [threadIconTarget, setThreadIconTarget] = createSignal<{ thread: Thread; history: string } | null>(null);
export function openThreadIconPicker(thread: Thread): void {
  // Freeze the displayed revision: a concurrent edit must be rejected by the Host.
  const history = historyId();
  if (history) setThreadIconTarget({ thread: { ...thread }, history });
}
