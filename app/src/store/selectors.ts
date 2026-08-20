import { EventKind } from "../protocol";
import type {
  EventItem,
  ProcessInfo,
  ProcessState,
  ViewInstance,
  ViewSpec,
} from "../protocol";
import type { EventOverride } from "./types";
// ---- Task collection (typed Events on the compatibility wire) ----

// ---- The optimistic override layer (one record, one projection) -------------

/** Do two `snoozed_until` values name the same moment? The Owner sends an
 * RFC3339 instant and the host round-trips it through its own clock type, so the
 * echo can be the same moment spelled differently ("…+00:00" vs "…Z"). Compared
 * as instants when both parse, as strings otherwise. */
function sameInstant(a: string | null | undefined, b: string | null | undefined): boolean {
  const left = a ?? null;
  const right = b ?? null;
  if (left === null || right === null) return left === right;
  const da = Date.parse(left);
  const db = Date.parse(right);
  if (Number.isFinite(da) && Number.isFinite(db)) return da === db;
  return left === right;
}

/** THE settle rule (R6-1): drop every assertion the wire truth already carries,
 * keeping only what is still genuinely optimistic. Returns `null` when nothing
 * is left to assert (or when `committed` is absent — an event the snapshot no
 * longer carries can have no pending local claim about it).
 *
 * The archive rule generalized: the archive override used to be pruned on a
 * committed `archived` upsert and on resync; every field now settles the same
 * way, on the same three occasions (write, `event_upsert`, `hello_ok`). */
export function settleOverride(
  override: EventOverride,
  committed: EventItem | undefined,
): EventOverride | null {
  if (!committed) return null;
  const next: EventOverride = {};
  if (override.decided !== undefined && (committed.status !== "open") !== override.decided) {
    next.decided = override.decided;
  }
  if (override.archived !== undefined && (committed.archived === true) !== override.archived) {
    next.archived = override.archived;
  }
  if (override.read !== undefined && (committed.read === true) !== override.read) {
    next.read = override.read;
  }
  if (
    override.snoozedUntil !== undefined &&
    !sameInstant(committed.snoozed_until, override.snoozedUntil)
  ) {
    next.snoozedUntil = override.snoozedUntil;
  }
  return Object.keys(next).length > 0 ? next : null;
}

/** Project one Event through its pending assertions.
 *
 * ALWAYS returns a fresh plain object, even for an un-overridden row. Returning
 * `event` itself would be free, but the projection is fed into a store of its
 * own: `reconcile` would then diff a live `state.events` proxy against itself
 * and patch the WIRE TRUTH in place the first time an assertion appeared —
 * exactly the optimistic layer leaking into the thing it must stay separable
 * from. The copy is what keeps the two stores disjoint. */
export function projectEvent(event: EventItem, override: EventOverride | undefined): EventItem {
  const projected: EventItem = { ...event };
  if (!override) return projected;
  if (override.decided !== undefined) projected.status = override.decided ? "done" : "open";
  if (override.archived !== undefined) projected.archived = override.archived;
  if (override.read !== undefined) projected.read = override.read;
  if (override.snoozedUntil !== undefined) projected.snoozed_until = override.snoozedUntil;
  return projected;
}

/** The Event list every surface reads: wire truth with the optimistic layer
 * folded in. Downstream selectors take these PLAIN projected events — no
 * surface, selector, or component threads override ids any more. */
export function projectEvents(
  events: EventItem[],
  overrides: Record<number, EventOverride>,
): EventItem[] {
  return events.map((event) => projectEvent(event, overrides[event.id]));
}

/** "Decided/resolved": the status is no longer open. Read off a PROJECTED
 * event, so an optimistic decide (not yet committed) counts exactly like a
 * committed one. One place so the queue partition, the counts, and the card
 * visuals all agree. */
export function isEventResolved(event: EventItem): boolean {
  return event.status !== "open";
}

/** "Archived" (archive contract v1), read off a PROJECTED event — the archive
 * twin of `isEventResolved`. One place so the default filter, the Archived(n)
 * count, and the archived view all agree. */
export function isEventArchived(event: EventItem): boolean {
  return event.archived === true;
}

/** Effective "snoozed" state for an Event (Wave-3 durable snooze): `snoozed_until`
 * is set and still in the future. One place so the Active filter,
 * the needs-you count, and the "Snoozed (n)" view all agree. Once the instant
 * passes the host clears the field and re-broadcasts, so the client never needs a
 * wall-clock timer to bring a return back. */
export function isEventSnoozed(event: EventItem, now: number = Date.now()): boolean {
  if (!event.snoozed_until) return false;
  const until = Date.parse(event.snoozed_until);
  return Number.isFinite(until) && until > now;
}

/** The resting queue: every event that is neither archived NOR snoozed. THE
 * lifecycle filter — an archived or snoozed event vanishes from every surface at
 * once and counts stay honest against the filtered set. Surfaces that render or
 * count TASKS go one step further, through `taskEvents`; this stays the honest
 * "everything still resting" set. `now` is injectable for deterministic tests. */
export function visibleEvents(events: EventItem[], now: number = Date.now()): EventItem[] {
  return events.filter((e) => !isEventArchived(e) && !isEventSnoozed(e, now));
}

/** THE Task set: the resting queue minus housekeeping `info` (ADR-0012's
 * quietest band — "session rotated" and friends). An info Event is a
 * notification, not work: its content already rides the conversation as an
 * inline message, so giving it a chip too would say the Owner has a Task where
 * he has only been told something. Every Task surface reads through this one
 * selector — the rail's chips, the load-time auto-focus, the needs-you count
 * and its badges, the palette's contextual actions — so no surface can disagree
 * with another about what a Task is. Info stays on the wire, in the
 * conversation, and in `visibleEvents` for any surface that deliberately lists
 * the whole resting queue. */
export function taskEvents(events: EventItem[], now: number = Date.now()): EventItem[] {
  return visibleEvents(events, now).filter((e) => e.kind !== EventKind.Info);
}

/** The set the "Clear finished (n)" sweep removes: finished (decided, or read
 * awareness) Tasks still resting in the queue — never touching what is still
 * open, snoozed, or already archived. Read off `taskEvents`, so the number on
 * the command is exactly the number of chips that disappear: sweeping an info
 * event the Owner was never shown would make the count lie. One selector so the
 * sweep's count and the ids it archives always agree. */
export function finishedEvents(events: EventItem[], now: number = Date.now()): EventItem[] {
  return taskEvents(events, now).filter(isEventFinished);
}

/** "Finished" per the archive contract: done, OR read awareness that never
 * needed a response — exactly the set `events.clear` sweeps, and the gate for
 * the card overflow's Archive action (an open judgment is archived only by the
 * host/agent, never offered the affordance here). */
export function isEventFinished(event: EventItem): boolean {
  return isEventResolved(event) || (event.read && !event.requires_response);
}

/** A judgment still needing the Owner: kind judgment, open, not optimistically
 * decided. The highest-priority Task state. */
export function isOpenJudgment(event: EventItem): boolean {
  return event.kind === EventKind.Judgment && !isEventResolved(event);
}

/** The ONE red on the surface (ADR-0012): the "N need you" count = open,
 * undecided judgments. Awareness never contributes. */
export function tasksNeedingOwnerCount(events: EventItem[]): number {
  return events.filter(isOpenJudgment).length;
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
function taskPriorityRank(event: EventItem): number {
  const open = isOpenJudgment(event);
  if (open && event.blocking) return 0;
  if (open) return 1;
  if (event.kind === EventKind.Judgment) return 2; // decided judgment, kept in place
  return 3; // awareness (summary/info)
}

/** Order the queue for the scroller: priority band first, oldest-waited first
 * within a band, id as a stable final tiebreak. Pure + total (never throws) so
 * the scroller and its tests share one ordering. */
export function orderedTasks(events: EventItem[]): EventItem[] {
  return events.slice().sort((a, b) => {
    const ra = taskPriorityRank(a);
    const rb = taskPriorityRank(b);
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

/** Count backing the Processes utility badge: running processes only.
 * Deliberately independent of Task attention state and document.title. */
export function runningProcessCount(processes: ProcessInfo[]): number {
  return processes.filter((p) => isProcessRunning(p.state)).length;
}

// ---- Generative-UI tier (view templates) ----

/** The Task id targeted by the legacy wire placement `ping:<id>`, or null.
 * Product surfaces call this a Task; only the persisted protocol spelling is
 * retained for backend compatibility. */
export function parseTaskPlacement(placement: string): number | null {
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

/** Views placed inline in the global conversation, in arrival order. The
 * `chat` string is a legacy wire value, not a product destination. */
export function conversationViews(views: ViewInstance[]): ViewInstance[] {
  return views.filter((v) => v.placement === "chat");
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
