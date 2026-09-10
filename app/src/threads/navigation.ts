import { createSignal } from "solid-js";
export type ThreadNavigationIntent = { kind: "browse" } | { kind: "create"; historyId: string; parentId: number | null };
/** The drawer alone consumes the opening intent and owns initial focus. */
export const [threadNavigationIntent, setThreadNavigationIntent] = createSignal<ThreadNavigationIntent | null>(null);
export const threadNavigationOpen = () => threadNavigationIntent() !== null;
export function openThreadNavigation(intent: ThreadNavigationIntent = { kind: "browse" }): void { setThreadNavigationIntent(intent); }
export function closeThreadNavigation(): void { setThreadNavigationIntent(null); }
