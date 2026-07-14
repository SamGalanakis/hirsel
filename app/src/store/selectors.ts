import type {
  EventItem,
  ProcessInfo,
  ProcessState,
  SideChatRef,
  ViewInstance,
  ViewSpec,
} from "../protocol";
// ---- Typed event queue (ADR-0012) ----

/** Effective "decided/resolved" state for an Event, folding in the optimistic
 * decide override (`eventDecideOverrides`):
 * resolved when the wire status is done OR the Owner has just decided it and the
 * ~5s Undo window has not yet committed. One place so the queue partition, the
 * counts, and the card visuals all agree. */
export function isEventResolved(event: EventItem, decideOverrides: number[]): boolean {
  return event.status !== "open" || decideOverrides.includes(event.id);
}

/** Effective "archived" state for an Event (archive contract v1), folding in
 * the optimistic archive override (`eventArchiveOverrides`) — the archive twin
 * of `isEventResolved`: archived when the wire flag is set OR the Owner has
 * just archived it and the host echo has not yet landed. One place so the
 * default filter, the Archived(n) count, and the archived view all agree. */
export function isEventArchived(event: EventItem, archiveOverrides: number[]): boolean {
  return event.archived === true || archiveOverrides.includes(event.id);
}

/** Effective "snoozed" state for an Event (Wave-3 durable snooze): `snoozed_until`
 * is set and still in the future. The optimistic flip lives on the event itself
 * (`event_snooze_local` patches `snoozed_until`), so — unlike archive/decide —
 * there is no separate override layer to fold in. One place so the Active filter,
 * the needs-you count, and the "Snoozed (n)" view all agree. Once the instant
 * passes the host clears the field and re-broadcasts, so the client never needs a
 * wall-clock timer to bring a return back. */
export function isEventSnoozed(event: EventItem, now: number = Date.now()): boolean {
  if (!event.snoozed_until) return false;
  const until = Date.parse(event.snoozed_until);
  return Number.isFinite(until) && until > now;
}

/** The resting queue: every event that is neither archived NOR snoozed. THE
 * default filter — every surface that renders or counts the queue (phone
 * scroller pages + pager counts, the desktop Feed column, the peek/queue list,
 * the phone nav badge, the needs-you count) reads through this, so an archived or
 * snoozed event vanishes everywhere at once and counts stay honest against the
 * filtered set. `now` is injectable for deterministic tests. */
export function visibleEvents(
  events: EventItem[],
  archiveOverrides: number[],
  now: number = Date.now(),
): EventItem[] {
  return events.filter((e) => !isEventArchived(e, archiveOverrides) && !isEventSnoozed(e, now));
}

/** The quiet "Snoozed (n)" view's data: non-archived snoozed events, soonest to
 * return first (a rising queue of things coming back). Each row shows its return
 * time + Unsnooze. `now` is injectable for deterministic tests. */
export function snoozedEvents(
  events: EventItem[],
  archiveOverrides: number[],
  now: number = Date.now(),
): EventItem[] {
  return events
    .filter((e) => !isEventArchived(e, archiveOverrides) && isEventSnoozed(e, now))
    .sort((a, b) => Date.parse(a.snoozed_until ?? "") - Date.parse(b.snoozed_until ?? ""));
}

/** The set the "Clear finished (n)" sweep removes: finished (decided, or read
 * awareness) events still resting in the queue — never touching what is still
 * open, snoozed, or already archived. One selector so the sweep's count and the
 * ids it archives always agree. */
export function finishedEvents(
  events: EventItem[],
  archiveOverrides: number[],
  decideOverrides: number[],
  now: number = Date.now(),
): EventItem[] {
  return visibleEvents(events, archiveOverrides, now).filter((e) =>
    isEventFinished(e, decideOverrides),
  );
}

/** The quiet Archived(n) view's data — the day-log (Wave-3 time axis): archived
 * events, newest-first by `archived_at` (the instant it was swept), falling back
 * to `id` order when the timestamp is absent (old data / a just-archived
 * optimistic row before the host echo). A missing `archived_at` sorts to the top
 * (a fresh sweep is the most recent thing). */
export function archivedEvents(events: EventItem[], archiveOverrides: number[]): EventItem[] {
  return events
    .filter((e) => isEventArchived(e, archiveOverrides))
    .sort((a, b) => {
      const ta = a.archived_at ? Date.parse(a.archived_at) : Number.POSITIVE_INFINITY;
      const tb = b.archived_at ? Date.parse(b.archived_at) : Number.POSITIVE_INFINITY;
      if (ta !== tb) return tb - ta;
      return b.id - a.id;
    });
}

/** "Finished" per the archive contract: done, OR read awareness that never
 * needed a response — exactly the set `events.clear` sweeps, and the gate for
 * the card overflow's Archive action (an open judgment is archived only by the
 * host/agent, never offered the affordance here). */
export function isEventFinished(event: EventItem, decideOverrides: number[]): boolean {
  return isEventResolved(event, decideOverrides) || (event.read && !event.requires_response);
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
 * judgments (kept in place for undo) → the awareness tail (summary/info). Lower
 * rank sorts earlier. (Wave-3: the old client-only "snoozed to the tail" band is
 * gone — durable snooze removes an event from the resting set entirely, so it
 * never reaches this ordering.) */
function queueRank(event: EventItem, decideOverrides: number[]): number {
  const open = isOpenJudgment(event, decideOverrides);
  if (open && event.blocking) return 0;
  if (open) return 1;
  if (event.kind === "judgment") return 2; // decided judgment, kept in place
  return 3; // awareness (summary/info)
}

/** Order the queue for the scroller: priority band first, oldest-waited first
 * within a band, id as a stable final tiebreak. Pure + total (never throws) so
 * the scroller and its tests share one ordering. */
export function orderedQueue(events: EventItem[], decideOverrides: number[]): EventItem[] {
  return events.slice().sort((a, b) => {
    const ra = queueRank(a, decideOverrides);
    const rb = queueRank(b, decideOverrides);
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
