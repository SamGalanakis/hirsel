import { historyId } from "../lib/history";
import { threadUrl, threadReference } from "../lib/thread-url";
import { openThreadIconPicker } from "./icon-picker";
import { openThreadCreate } from "./create";
import { threadAction } from "./store";
import { getClient } from "../ws/client";
import { toast } from "../lib/toast";
import type { Thread } from "./types";
export type ThreadActionIcon = "settle" | "reopen" | "read" | "snooze" | "archive" | "copy" | "stop" | "pin" | "child" | "icon" | "kind";
/** Three questions a Thread menu answers, in this order: what this Thread is,
 * what its work is doing, and who sees it. The menu draws a separator wherever
 * the group changes, so eight items read as three decisions. */
export type ThreadActionGroup = "identity" | "work" | "visibility";
export interface ThreadActionOption { id: string; label: string; run: () => void }
export interface ThreadActionItem {
  id: string;
  label: string;
  icon: ThreadActionIcon;
  group: ThreadActionGroup;
  /** Archive hides work the Owner may still need; it is styled as the one
   * consequential item rather than sitting flush with "Copy link". */
  destructive?: boolean;
  /** A choice this action needs before it can run, offered as a submenu. */
  options?: ThreadActionOption[];
  run: () => void;
}
/** Durations the Owner actually snoozes for, resolved against the caller's now. */
function snoozeOptions(now: number, wake: (until: string) => void): ThreadActionOption[] {
  const at = (ms: number) => new Date(now + ms).toISOString();
  const tomorrow = () => { const date = new Date(now); date.setDate(date.getDate() + 1); date.setHours(9, 0, 0, 0); return date.toISOString(); };
  return [
    { id: "snooze-1h", label: "For an hour", run: () => wake(at(3_600_000)) },
    { id: "snooze-3h", label: "For three hours", run: () => wake(at(10_800_000)) },
    { id: "snooze-tomorrow", label: "Until tomorrow morning", run: () => wake(tomorrow()) },
    { id: "snooze-week", label: "For a week", run: () => wake(at(604_800_000)) },
  ];
}
/** The same supported actions power the row, context strip and command palette.
 * Every label names this Thread by its own kind — a Space is never called a
 * task halfway down its own menu. */
export function threadActions(thread: Thread, now = Date.now()): ThreadActionItem[] {
  const referenceHistory = historyId();
  const noun = thread.kind === "space" ? "Space" : "Task";
  const send = (name: string, data: unknown = {}, revision?: number) => { if (referenceHistory) threadAction(referenceHistory, thread.id, name, data, revision); };
  const actions: ThreadActionItem[] = [
    { id: "icon", label: `Change ${noun} icon`, icon: "icon", group: "identity", run: () => openThreadIconPicker(thread) },
    { id: "child", label: thread.kind === "task" ? "New child Task" : "New child", icon: "child", group: "identity", run: () => openThreadCreate(thread.id) },
  ];
  if (thread.parent_thread_id === null) actions.splice(1, 0, { id: "pin", label: `${thread.pinned_at ? "Unpin" : "Pin"} ${noun}`, icon: "pin", group: "identity", run: () => send(thread.pinned_at ? "unpin" : "pin", {}, thread.revision) });
  if (thread.kind === "space") actions.push({ id: "set-kind", label: "Change to Task", icon: "kind", group: "identity", run: () => send("set_kind", { kind: "task" }, thread.revision) });
  else if (!thread.settled_at) actions.push({ id: "set-kind", label: "Change to Space", icon: "kind", group: "identity", run: () => send("set_kind", { kind: "space" }, thread.revision) });
  if (thread.kind === "task") actions.push({ id: "settle", label: thread.settled_at ? `Reopen ${noun}` : `Mark ${noun} done`, icon: thread.settled_at ? "reopen" : "settle", group: "work", run: () => send(thread.settled_at ? "reopen" : "settle") });
  if (thread.running_turn) actions.push({ id: "stop", label: "Stop current turn", icon: "stop", group: "work", run: () => { if (referenceHistory) getClient()?.cancelTurn(referenceHistory, thread.id); } });
  if (!thread.read) actions.push({ id: "read", label: "Mark read", icon: "read", group: "work", run: () => send("read") });
  const snoozed = Boolean(thread.snoozed_until && Date.parse(thread.snoozed_until) > now);
  actions.push(snoozed
    ? { id: "snooze", label: `Unsnooze ${noun}`, icon: "snooze", group: "visibility", run: () => send("unsnooze") }
    : { id: "snooze", label: `Snooze ${noun}`, icon: "snooze", group: "visibility", options: snoozeOptions(now, until => send("snooze", { until })), run: () => send("snooze", { until: new Date(now + 86_400_000).toISOString() }) });
  actions.push({ id: "archive", label: `${thread.archived_at ? "Unarchive" : "Archive"} ${noun}`, icon: "archive", group: "visibility", destructive: !thread.archived_at, run: () => send(thread.archived_at ? "unarchive" : "archive") });
  actions.push({ id: "copy", label: `Copy ${noun} link`, icon: "copy", group: "visibility", run: () => { void (async () => {
    try { await navigator.clipboard.writeText(threadUrl({kind:"thread",history_id:referenceHistory!,thread_id:thread.id})); toast("Thread link copied"); }
    catch { toast("Couldn’t copy the link. Open the thread and copy its address.", { variant: "error" }); }
  })(); } });
  actions.push({ id: "copy-reference", label: "Copy reference", icon: "copy", group: "visibility", run: () => { void (async () => {
    try { await navigator.clipboard.writeText(threadReference({kind:"thread",history_id:referenceHistory!,thread_id:thread.id})); toast("Thread reference copied"); }
    catch { toast("Couldn’t copy the reference.", {variant:"error"}); }
  })(); } });
  return actions;
}
