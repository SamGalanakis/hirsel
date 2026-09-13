import { createSignal } from "solid-js";
export type ThreadNavigationIntent = { kind: "browse" };
/** The drawer alone consumes the opening intent and owns initial focus. */
export const [threadNavigationIntent, setThreadNavigationIntent] = createSignal<ThreadNavigationIntent | null>(null);
export const threadNavigationOpen = () => threadNavigationIntent() !== null;
export function openThreadNavigation(intent: ThreadNavigationIntent = { kind: "browse" }): void { setThreadNavigationIntent(intent); }
export function closeThreadNavigation(): void { setThreadNavigationIntent(null); }
/** Where Back goes when the conversation is already showing: the Threads this
 * session visited, most recent last. The shell records every focus change; a
 * revisit moves its Thread to the top rather than growing the stack, so Back
 * walks distinct destinations instead of oscillating between two. Returning to
 * the overview forgets the trail — from there Back has nowhere left to go. */
const [threadVisits, setThreadVisits] = createSignal<number[]>([]);
export function recordThreadVisit(id: number | null): void {
  setThreadVisits(stack => {
    if (id === null) return stack.length === 0 ? stack : [];
    if (stack[stack.length - 1] === id) return stack;
    return [...stack.filter(visited => visited !== id), id].slice(-20);
  });
}
/** The Thread Back would return to, or null when this is the first of the session. */
export function previousThread(): number | null {
  const stack = threadVisits();
  return stack.length > 1 ? stack[stack.length - 2] : null;
}
/** Drop the current Thread from the trail and report where Back lands. */
export function popThreadVisit(): number | null {
  const target = previousThread();
  if (target !== null) setThreadVisits(stack => stack.slice(0, -1));
  return target;
}
