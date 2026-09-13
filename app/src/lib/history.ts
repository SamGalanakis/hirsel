import { createSignal } from "solid-js";
const HISTORY_KEY = "hirsel.history-id";
export const [historyId, setHistoryId] = createSignal<string | null>(null);
/** A reset drops every draft addressed to the previous history: text never follows a reused numeric Thread ID. */
export function acceptHistory(id: string): boolean {
  if (typeof id !== "string" || id.length === 0) throw new Error("Host did not provide a history identity.");
  const previous = historyId() ?? localStorage.getItem(HISTORY_KEY);
  localStorage.setItem(HISTORY_KEY, id); setHistoryId(id);
  const reset = previous !== null && previous !== id;
  if (reset) dropForeignDrafts(id);
  return reset;
}
function dropForeignDrafts(current: string): void {
  const stale: string[] = [];
  for (let index = 0; index < localStorage.length; index++) {
    const key = localStorage.key(index);
    if (key?.startsWith("hirsel.draft.") && !key.startsWith(`hirsel.draft.${current}:`)) stale.push(key);
  }
  for (const key of stale) localStorage.removeItem(key);
}
