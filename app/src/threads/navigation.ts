import { createSignal } from "solid-js";

/** One drawer state for the rail, keyboard shortcut and command palette. */
export const [threadNavigationOpen, setThreadNavigationOpen] = createSignal(false);
export function openThreadNavigation(): void { setThreadNavigationOpen(true); }
