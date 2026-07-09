import type { Ping, ProcessInfo, ProcessState, SideChatRef } from "../protocol";
import type { DisplayMessage } from "./types";

/** v2.1 (ADR-0009): a Ping is "resolved" (rendered under Done) when its status
 * is anything other than open. Both the new `done` value and the legacy
 * `archived` spelling count — one terminal state, two wire spellings. Typed
 * loosely so the legacy value tolerates through. Kept in one place so the Tray
 * shelf, the Done section, and the card visuals all agree. */
export function isResolvedStatus(status: string): boolean {
  return status !== "open";
}

/** Effective "seen" state for a Ping (v1.3). A Ping is read when the wire
 * `read` flag is true AND the Owner has not manually "Marked unread" it (a
 * client-only override). Kept in one place so the badge, the card visual
 * state, and the auto-read gate all agree by construction. */
export function isPingRead(ping: Ping, unreadOverrides: number[]): boolean {
  return ping.read === true && !unreadOverrides.includes(ping.id);
}

/** Count backing both the Tray shelf badge and the document.title badge. v1.3:
 * email-like "unread" count = open Pings that are not yet effectively read
 * (was open + requires_response). requires_response no longer affects the
 * badge count — it only drives the card accent and (Tray, v1.6) the shelf
 * badge's tone. */
export function openUnreadCount(pings: Ping[], unreadOverrides: number[]): number {
  return pings.filter((p) => p.status === "open" && !isPingRead(p, unreadOverrides)).length;
}

/** Tray (v1.6): true when any open Ping still requires a response — drives the
 * shelf badge's `status-danger` accent (muted neutral otherwise). Independent
 * of the unread count so a Ping can be read but still awaiting a reply. */
export function hasOpenRequiresResponse(pings: Ping[]): boolean {
  return pings.some((p) => p.status === "open" && p.requires_response);
}

/** Tray (v1.6): the single Ping the collapsed shelf previews — the most
 * "actionable" open Ping, in order: newest open `requires_response`, else
 * newest unread, else newest open. "Newest" = highest host-assigned id (ids
 * are monotonic), matching the ordering convention used elsewhere (e.g.
 * PingsView, partitionProcesses). Null when there are no open Pings. */
export function mostActionablePing(
  pings: Ping[],
  unreadOverrides: number[],
): Ping | null {
  const open = pings.filter((p) => p.status === "open").sort((a, b) => b.id - a.id);
  if (open.length === 0) return null;
  return (
    open.find((p) => p.requires_response) ??
    open.find((p) => !isPingRead(p, unreadOverrides)) ??
    open[0]
  );
}

/** The Owner's reply to a Ping, derived — never persisted — from Chat. A reply
 * is just an anchor-refed owner Chat message (`ref === ping.anchor`), so its
 * lifecycle (optimistic `pending` → echoed `✓`, or `failed`) is exactly the
 * send it already is. */
export interface PingReply {
  body: string;
  pending: boolean;
  failed: boolean;
}

/** Latest Owner reply anchored to `anchor`, or null if none yet. Drives a Ping
 * card's inline "you: … ✓" replied state. Derived from Chat messages so there
 * is no new Ping reply state to persist or reconcile; when the send reconciles
 * from optimistic to host echo, `pending` flips to false by itself. Newest-first
 * scan: if the Owner answers the same Ping twice, the most recent reply wins. */
export function latestReplyForAnchor(
  messages: DisplayMessage[],
  anchor: number,
): PingReply | null {
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

/** The live side chat for a Ping, if any — drives the "in progress · resume"
 * affordance on its card in place of the plain "Discuss" entry. Derived from
 * `hello_ok.side_chats` + open/closed tracking (`sideChatRefs`), not from the
 * (possibly never-hydrated) `sideChats` map. */
export function sideChatForPing(refs: SideChatRef[], pingId: number): SideChatRef | null {
  return refs.find((r) => r.ping_id === pingId) ?? null;
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
