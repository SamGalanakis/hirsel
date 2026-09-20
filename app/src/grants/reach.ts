import { createSignal } from "solid-js";
import { historyId } from "../lib/history";
import type { Thread } from "../threads/types";

/** Reach changes rarely, so it lives one level in: the Thread's actions menu
 * opens this modal, which owns the whole reach list while it is open. */
export const [threadReachTarget, setThreadReachTarget] = createSignal<{ thread: Thread; history: string } | null>(null);
export function openThreadReach(thread: Thread): void {
  const history = historyId();
  if (history) setThreadReachTarget({ thread: { ...thread }, history });
}
export function closeThreadReach(): void { setThreadReachTarget(null); }
/** The dialog's own name, so the Owner always knows whose reach they are editing. */
export function reachDialogTitle(thread: Thread): string {
  return `Reach of ${thread.kind === "space" ? "Space" : "Task"} #${thread.id} “${thread.title}”`;
}
