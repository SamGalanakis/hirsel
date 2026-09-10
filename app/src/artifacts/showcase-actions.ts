import { createSignal } from "solid-js";
import { historyId } from "../lib/history";
import { state } from "../store/store";
import { threadAction, threadState } from "../threads/store";
import type { RelatedOrigin } from "../related/store";

export interface ShowcaseOrigin extends RelatedOrigin { readonly revision: number; readonly title: string }
export function captureShowcaseOrigin(origin: RelatedOrigin | null): ShowcaseOrigin | null {
  if (!origin || origin.historyId !== historyId() || !threadState.ready) return null;
  const thread = threadState.threads.find(thread => thread.id === origin.threadId);
  return thread ? { ...origin, revision: thread.revision, title: thread.title } : null;
}
export function setThreadShowcase(origin: ShowcaseOrigin, artifactId: number | null): void {
  if (origin.historyId !== historyId() || !threadState.ready || state.connection !== "connected") throw new Error("The history or connection changed. Reopen this control and try again.");
  const thread = threadState.threads.find(thread => thread.id === origin.threadId);
  if (!thread || thread.revision !== origin.revision) throw new Error("This thread changed. Reopen this control and try again.");
  threadAction(origin.threadId, "set_showcase", { artifact_id: artifactId, history_id: origin.historyId }, origin.revision);
}
export const [showcasePicker, setShowcasePicker] = createSignal<ShowcaseOrigin | null>(null);
export const [phoneShowcase, setPhoneShowcase] = createSignal<RelatedOrigin | null>(null);
