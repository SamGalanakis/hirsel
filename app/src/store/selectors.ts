import type { InboxItem, ProcessInfo, ProcessState, SideChatRef } from "../protocol";
import type { DisplayMessage } from "./types";

/** Effective "seen" state for an Inbox item (v1.3). An item is read when the
 * wire `read` flag is true AND the Owner has not manually "Marked unread" it
 * (a client-only override). Kept in one place so the badge, the card visual
 * state, and the auto-read gate all agree by construction. */
export function isItemRead(item: InboxItem, unreadOverrides: number[]): boolean {
  return item.read === true && !unreadOverrides.includes(item.id);
}

/** Count backing both the Tray shelf badge and the document.title badge. v1.3:
 * email-like "unread" count = open items that are not yet effectively read
 * (was open + requires_response). requires_response no longer affects the
 * badge count — it only drives the card accent and (Tray, v1.6) the shelf
 * badge's tone. */
export function openUnreadCount(inbox: InboxItem[], unreadOverrides: number[]): number {
  return inbox.filter((i) => i.status === "open" && !isItemRead(i, unreadOverrides)).length;
}

/** Tray (v1.6): true when any open item still requires a response — drives the
 * shelf badge's `status-danger` accent (muted neutral otherwise). Independent
 * of the unread count so an item can be read but still awaiting a reply. */
export function hasOpenRequiresResponse(inbox: InboxItem[]): boolean {
  return inbox.some((i) => i.status === "open" && i.requires_response);
}

/** Tray (v1.6): the single item the collapsed shelf previews — the most
 * "actionable" open item, in order: newest open `requires_response`, else
 * newest unread, else newest open. "Newest" = highest host-assigned id (ids
 * are monotonic), matching the ordering convention used elsewhere (e.g.
 * InboxView, partitionProcesses). Null when there are no open items. */
export function mostActionableItem(
  inbox: InboxItem[],
  unreadOverrides: number[],
): InboxItem | null {
  const open = inbox.filter((i) => i.status === "open").sort((a, b) => b.id - a.id);
  if (open.length === 0) return null;
  return (
    open.find((i) => i.requires_response) ??
    open.find((i) => !isItemRead(i, unreadOverrides)) ??
    open[0]
  );
}

/** The Owner's reply to an Inbox Item, derived — never persisted — from Chat.
 * A reply is just an anchor-refed owner Chat message (`ref === item.anchor`),
 * so its lifecycle (optimistic `pending` → echoed `✓`, or `failed`) is exactly
 * the send it already is. */
export interface ItemReply {
  body: string;
  pending: boolean;
  failed: boolean;
}

/** Latest Owner reply anchored to `anchor`, or null if none yet. Drives an
 * Inbox card's inline "you: … ✓" replied state. Derived from Chat messages so
 * there is no new inbox reply state to persist or reconcile; when the send
 * reconciles from optimistic to host echo, `pending` flips to false by itself.
 * Newest-first scan: if the Owner answers the same item twice, the most recent
 * reply wins. */
export function latestReplyForAnchor(
  messages: DisplayMessage[],
  anchor: number,
): ItemReply | null {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.author === "owner" && m.ref === anchor) {
      return { body: m.body, pending: m.pending === true, failed: m.failed === true };
    }
  }
  return null;
}

// ---- v1.4 processes ----

/** `running` is the only non-terminal state; everything else is Finished. */
export function isProcessRunning(state: ProcessState): boolean {
  return state === "running";
}

/** Count backing the Processes tab badge (v1.4): running processes only.
 * Deliberately independent of the Inbox unread badge and document.title. */
export function runningProcessCount(processes: ProcessInfo[]): number {
  return processes.filter((p) => isProcessRunning(p.state)).length;
}

// ---- v2.0 side chats (ADR-0008) ----

/** The live side chat for an Inbox item, if any — drives the "in progress ·
 * resume" affordance on its card in place of the plain "Discuss" entry.
 * Derived from `hello_ok.side_chats` + open/closed tracking (`sideChatRefs`),
 * not from the (possibly never-hydrated) `sideChats` map. */
export function sideChatForItem(refs: SideChatRef[], itemId: number): SideChatRef | null {
  return refs.find((r) => r.item_id === itemId) ?? null;
}

/** Group processes into Running / Finished, each newest-activity-first
 * (`last_event_ts` desc, id as a stable tiebreak). One place so the tab list
 * and any future surfaces agree on ordering. */
export function partitionProcesses(processes: ProcessInfo[]): {
  running: ProcessInfo[];
  finished: ProcessInfo[];
} {
  const byActivity = (a: ProcessInfo, b: ProcessInfo) => {
    const d = Date.parse(b.last_event_ts) - Date.parse(a.last_event_ts);
    return d !== 0 ? d : a.id < b.id ? 1 : -1;
  };
  const running = processes.filter((p) => isProcessRunning(p.state)).sort(byActivity);
  const finished = processes.filter((p) => !isProcessRunning(p.state)).sort(byActivity);
  return { running, finished };
}
