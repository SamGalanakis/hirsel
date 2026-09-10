import { createSignal } from "solid-js";
const HISTORY_KEY = "hirsel.history-id";
export const [historyId, setHistoryId] = createSignal<string | null>(null);
/** A reset never silently addresses saved text to a reused numeric Thread ID. */
export function acceptHistory(id: string): boolean {
  if (typeof id !== "string" || id.length === 0) throw new Error("Host did not provide a history identity.");
  const previous = historyId() ?? localStorage.getItem(HISTORY_KEY);
  localStorage.setItem(HISTORY_KEY, id); setHistoryId(id);
  return previous !== null && previous !== id;
}
export function preservePendingDrafts(drafts: { clientId: string; body: string }[]): void {
  for (const draft of drafts) if (draft.body) localStorage.setItem(`hirsel.draft.recovered-${draft.clientId}`, draft.body);
}
export function recoveredDrafts(): { key: string; text: string }[] {
  const current = historyId(); if (!current) return [];
  const rows: { key: string; text: string }[] = [];
  for (let index = 0; index < localStorage.length; index++) {
    const key = localStorage.key(index);
    if (key?.startsWith("hirsel.draft.") && !key.startsWith(`hirsel.draft.${current}:`)) {
      const text = localStorage.getItem(key); if (text) rows.push({ key, text });
    }
  }
  return rows;
}
