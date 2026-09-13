import { createSignal } from "solid-js";
/** One creation surface for the whole app: the rail, the inventory header, the
 * palette action, a row's "New child" and the empty state all open this modal,
 * which owns the draft until it is sent or dismissed. */
export interface ThreadCreateIntent { parentId: number | null }
const [threadCreateIntent, setThreadCreateIntent] = createSignal<ThreadCreateIntent | null>(null);
export { threadCreateIntent };
export const threadCreateOpen = () => threadCreateIntent() !== null;
export function openThreadCreate(parentId: number | null = null): void { setThreadCreateIntent({ parentId }); }
export function closeThreadCreate(): void { setThreadCreateIntent(null); }
