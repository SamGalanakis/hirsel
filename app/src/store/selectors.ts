import type {
  EventItem,
  Ping,
  ProcessInfo,
  ProcessState,
  SideChatRef,
  ViewInstance,
  ViewSpec,
} from "../protocol";
import type { DisplayMessage } from "./types";

/** v2.1 (ADR-0009): a Ping is "resolved" (rendered under Done) when its status
 * is anything other than open. Both the new `done` value and the legacy
 * `archived` spelling count — one terminal state, two wire spellings. Typed
 * loosely so the legacy value tolerates through. Kept in one place so the Tray
 * shelf, the Done section, and the card visuals all agree. */
export function isResolvedStatus(status: string): boolean {
  return status !== "open";
}

/** Effective "resolved" (rendered under Done) state for a Ping, folding in the
 * client-only optimistic Mark-done override (`resolveOverrides`): a Ping is
 * resolved when its wire status is resolved OR the Owner has just Marked it
 * done and the 5s Undo window has not yet committed. Kept in one place so the
 * open/Done partition, the counts, and the card visuals all agree — the same
 * role `isPingRead` plays for the read override. */
export function isPingResolved(ping: Ping, resolveOverrides: number[]): boolean {
  return isResolvedStatus(ping.status) || resolveOverrides.includes(ping.id);
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
export function openUnreadCount(
  pings: Ping[],
  unreadOverrides: number[],
  resolveOverrides: number[] = [],
): number {
  return pings.filter(
    (p) =>
      p.status === "open" &&
      !resolveOverrides.includes(p.id) &&
      !isPingRead(p, unreadOverrides),
  ).length;
}

/** Tray (v1.6): true when any open Ping still requires a response — drives the
 * shelf badge's `status-danger` accent (muted neutral otherwise). Independent
 * of the unread count so a Ping can be read but still awaiting a reply. */
export function hasOpenRequiresResponse(
  pings: Ping[],
  resolveOverrides: number[] = [],
): boolean {
  return pings.some(
    (p) => p.status === "open" && p.requires_response && !resolveOverrides.includes(p.id),
  );
}

/** Tray (v1.6): the single Ping the collapsed shelf previews — the most
 * "actionable" open Ping, in order: newest open `requires_response`, else
 * newest unread, else newest open. "Newest" = highest host-assigned id (ids
 * are monotonic), matching the ordering convention used elsewhere (e.g.
 * PingsView, partitionProcesses). Null when there are no open Pings. */
export function mostActionablePing(
  pings: Ping[],
  unreadOverrides: number[],
  resolveOverrides: number[] = [],
): Ping | null {
  const open = pings
    .filter((p) => p.status === "open" && !resolveOverrides.includes(p.id))
    .sort((a, b) => b.id - a.id);
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

// ---- Typed event queue (ADR-0012), generalizing the ping selectors above ----

/** Effective "decided/resolved" state for an Event, folding in the optimistic
 * decide override (`eventDecideOverrides`) — the event twin of `isPingResolved`:
 * resolved when the wire status is done OR the Owner has just decided it and the
 * ~5s Undo window has not yet committed. One place so the queue partition, the
 * counts, and the card visuals all agree. */
export function isEventResolved(event: EventItem, decideOverrides: number[]): boolean {
  return event.status !== "open" || decideOverrides.includes(event.id);
}

/** A judgment still needing the Owner: kind judgment, open, not optimistically
 * decided. The queue's hero rank. */
export function isOpenJudgment(event: EventItem, decideOverrides: number[]): boolean {
  return event.kind === "judgment" && !isEventResolved(event, decideOverrides);
}

/** The ONE red on the surface (ADR-0012): the "N need you" count = open,
 * undecided judgments. Awareness never contributes. */
export function openJudgmentCount(events: EventItem[], decideOverrides: number[]): number {
  return events.filter((e) => isOpenJudgment(e, decideOverrides)).length;
}

/** Wait time drives ordering within the judgment band — oldest first is the most
 * blocking. Older ts sorts earlier; unparseable ts sinks to the end. */
function tsAsc(a: EventItem, b: EventItem): number {
  const da = Date.parse(a.ts);
  const db = Date.parse(b.ts);
  const na = Number.isFinite(da) ? da : Number.POSITIVE_INFINITY;
  const nb = Number.isFinite(db) ? db : Number.POSITIVE_INFINITY;
  return na - nb;
}

/** The queue rank (ADR-0012 interrupt-vs-accrue invariant, expressed as
 * ordering): blocking judgments first → other needs-you judgments → decided
 * judgments (kept in place for undo) → the awareness tail (summary/info).
 * `snoozed` ids (a client-only "sent to the tail" gesture) drop to just above
 * awareness. Lower rank sorts earlier. */
function queueRank(event: EventItem, decideOverrides: number[], snoozed: Set<number>): number {
  const open = isOpenJudgment(event, decideOverrides);
  if (open && event.blocking && !snoozed.has(event.id)) return 0;
  if (open && !snoozed.has(event.id)) return 1;
  if (open && snoozed.has(event.id)) return 2;
  if (event.kind === "judgment") return 3; // decided judgment, kept in place
  return 4; // awareness (summary/info)
}

/** Order the queue for the scroller: priority band first, oldest-waited first
 * within a band, id as a stable final tiebreak. Pure + total (never throws) so
 * the scroller and its tests share one ordering. */
export function orderedQueue(
  events: EventItem[],
  decideOverrides: number[],
  snoozed: Set<number> = new Set(),
): EventItem[] {
  return events.slice().sort((a, b) => {
    const ra = queueRank(a, decideOverrides, snoozed);
    const rb = queueRank(b, decideOverrides, snoozed);
    if (ra !== rb) return ra - rb;
    const t = tsAsc(a, b);
    if (t !== 0) return t;
    return a.id - b.id;
  });
}

/** The `ui` field normalized to an array of nodes: the wire carries either the
 * blessed `{type:"card",children:[…]}` root or a bare array of nodes (the
 * spikes). A card root unwraps to its children; an array passes through; a
 * single non-card node wraps into a one-element list; anything malformed → []. */
export function eventUiNodes(ui: ViewSpec | ViewSpec[] | undefined): ViewSpec[] {
  if (Array.isArray(ui)) return ui.filter((n) => n && typeof n.type === "string");
  if (ui && typeof ui.type === "string") {
    if (ui.type === "card" && Array.isArray(ui.children)) {
      return (ui.children as ViewSpec[]).filter((n) => n && typeof n.type === "string");
    }
    return [ui];
  }
  return [];
}

/** A one-line title for an Event, for the peek overview / accessibility: the
 * first heading, else the first status/text label, else the `@`-name. Backtick
 * tokens are stripped (they're a render hint, not content). */
export function eventTitle(event: EventItem): string {
  const nodes = eventUiNodes(event.ui);
  const pick = (type: string, field: string): string | null => {
    const n = nodes.find((x) => x.type === type);
    const v = n ? (n as Record<string, unknown>)[field] : null;
    return typeof v === "string" ? v.replace(/`/g, "") : null;
  };
  return (
    pick("heading", "text") ??
    pick("status", "label") ??
    pick("text", "text") ??
    (event.description || event.name)
  );
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

// ---- Generative-UI tier (view templates) ----

/** The numeric Ping id a `ping:<id>` placement targets, or null for any other
 * placement (`canvas` / `chat`) or a malformed value. One parser so the Ping
 * card and the placement routing agree on what "belongs to this Ping" means. */
export function parsePingPlacement(placement: string): number | null {
  const m = /^ping:(\d+)$/.exec(placement);
  if (!m) return null;
  const id = Number(m[1]);
  return Number.isFinite(id) ? id : null;
}

/** Views placed on the shared Canvas surface, oldest-first (insertion order).
 * The Canvas auto-surfaces the newest, i.e. the LAST of this list. */
export function canvasViews(views: ViewInstance[]): ViewInstance[] {
  return views.filter((v) => v.placement === "canvas");
}

/** Views placed inline in the chat transcript, in arrival order. */
export function chatViews(views: ViewInstance[]): ViewInstance[] {
  return views.filter((v) => v.placement === "chat");
}

/** Views that belong inside a given Ping's card (`ping:<id>` placement). */
export function viewsForPing(views: ViewInstance[], pingId: number): ViewInstance[] {
  return views.filter((v) => parsePingPlacement(v.placement) === pingId);
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
