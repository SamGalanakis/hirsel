import { threadAction } from "./store";
import { getClient } from "../ws/client";
import { toast } from "../lib/toast";
import type { Thread } from "./types";
export type ThreadActionIcon = "settle" | "reopen" | "read" | "snooze" | "archive" | "copy" | "stop";
export interface ThreadActionItem { id: string; label: string; icon: ThreadActionIcon; run: () => void }
/** The same supported actions power the row, context strip and command palette. */
export function threadActions(thread: Thread, now = Date.now()): ThreadActionItem[] {
  const actions: ThreadActionItem[] = [];
  if (thread.id !== 0) actions.push({ id: "settle", label: thread.settled_at ? "Reopen thread" : "Settle thread", icon: thread.settled_at ? "reopen" : "settle", run: () => threadAction(thread.id, thread.settled_at ? "reopen" : "settle") });
  if (!thread.read) actions.push({ id: "read", label: "Mark read", icon: "read", run: () => threadAction(thread.id, "read") });
  if (thread.id !== 0) {
    const snoozed = Boolean(thread.snoozed_until && Date.parse(thread.snoozed_until) > now);
    actions.push({ id: "snooze", label: snoozed ? "Unsnooze" : "Snooze for a day", icon: "snooze", run: () => threadAction(thread.id, snoozed ? "unsnooze" : "snooze", snoozed ? {} : { until: new Date(Date.now() + 86_400_000).toISOString() }) });
    actions.push({ id: "archive", label: thread.archived_at ? "Unarchive thread" : "Archive thread", icon: "archive", run: () => threadAction(thread.id, thread.archived_at ? "unarchive" : "archive") });
  }
  actions.push({ id: "copy", label: "Copy thread link", icon: "copy", run: () => { void (async () => {
    try { await navigator.clipboard.writeText(new URL(thread.id === 0 ? "/" : `/t/${thread.id}`, location.origin).href); toast("Thread link copied"); }
    catch { toast("Couldn’t copy the link. Open the thread and copy its address.", { variant: "error" }); }
  })(); } });
  if (thread.running_turn) actions.push({ id: "stop", label: "Stop current turn", icon: "stop", run: () => getClient()?.cancelTurn(thread.id) });
  return actions;
}
