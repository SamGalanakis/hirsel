// Pure reducer over the client's protocol-facing state. Kept free of any
// WebSocket concerns so it can be unit tested directly (see reducer.test.ts)
// and reused verbatim by the Solid store.
import type { EventItem, ProcessInfo, ViewInstance } from "../protocol";
import { settleOverride } from "./selectors";
import type {
  Action,
  AppState,
  DisplayMessage,
  EventOverride,
  PendingSend,
  TimelineEvent,
  Upload,
} from "./types";

/** Cap on retained finished-turn timelines (session memory for "turn details").
 * A live conversation has few turns; this only guards a pathological long run. */
const TURN_DETAILS_LIMIT = 50;

/** Hard cap on the in-memory conversation history (D10). An always-open PWA would
 * otherwise grow `messages` without bound across a multi-day session — every
 * append is O(n) to render and the unseen scan is O(n) per scroll. We keep the
 * most-recent slice; older rows fall out of memory (the host still holds the
 * canonical history, replayed from `last_seen` on reconnect). Comfortably larger
 * than the visible conversation window so "load older" has headroom. Oldest are
 * dropped from the FRONT, so optimistic sends (always newest, at the tail) and
 * the reconciliation that matches them are never disturbed. */
export const MESSAGES_CAP = 600;

/** Keep only the newest `MESSAGES_CAP` messages, dropping the oldest. A no-op
 * (same reference) under the cap so the common case allocates nothing. */
function capMessages(messages: DisplayMessage[]): DisplayMessage[] {
  return messages.length > MESSAGES_CAP ? messages.slice(messages.length - MESSAGES_CAP) : messages;
}

/** Backfill grows at the oldest edge, where the reader is. If the bounded
 * range fills, discard the newest committed rows instead; optimistic sends are
 * never evicted. The range remains contiguous and `hasLaterMessages` makes the
 * jump affordance reload the true newest page before pinning. */
function capMessagesKeepingOldest(messages: DisplayMessage[]): DisplayMessage[] {
  const committed = messages.filter((message) => !message.pending);
  const pending = messages.filter((message) => message.pending);
  const committedBudget = Math.max(1, MESSAGES_CAP - pending.length);
  if (committed.length <= committedBudget) return messages;
  return [...committed.slice(0, committedBudget), ...pending];
}

/** Upsert an Event by id, preserving list position for a known id (the queue
 * ordering is applied by the selector, not stored). */
function upsertEvent(events: EventItem[], event: EventItem): EventItem[] {
  const idx = events.findIndex((e) => e.id === event.id);
  if (idx === -1) return [...events, event];
  const next = events.slice();
  next[idx] = event;
  return next;
}

/** Cap on the optimistic Event override record. A gesture settles within a
 * round-trip, so this only guards a pathological offline burst; the
 * lowest-numbered (oldest) events are shed first. */
const EVENT_OVERRIDES_LIMIT = 200;

function capOverrides(
  overrides: Record<number, EventOverride>,
): Record<number, EventOverride> {
  const ids = Object.keys(overrides).map(Number);
  if (ids.length <= EVENT_OVERRIDES_LIMIT) return overrides;
  const next = { ...overrides };
  for (const id of ids.sort((a, b) => a - b).slice(0, ids.length - EVENT_OVERRIDES_LIMIT)) {
    delete next[id];
  }
  return next;
}

/** A gesture can land before the event itself does (a decide replayed against a
 * store the snapshot has not reached yet). Settling such an assertion against
 * "nothing known" would drop it, so it settles against wire DEFAULTS instead —
 * open, unarchived, unread, un-snoozed — and only a real `event_upsert` /
 * `hello_ok` can retire it. */
const PHANTOM_EVENT = {
  status: "open",
  archived: false,
  read: false,
  snoozed_until: null,
} as EventItem;

/** Record an optimistic assertion for one event, immediately settled against
 * the wire truth: a gesture that only restates what `events` already says
 * leaves no entry behind (so `unarchive` on a never-committed archive, or
 * `read` on an already-read event, is a true no-op). */
function assertOverride(
  state: AppState,
  eventId: number,
  patch: EventOverride,
): Record<number, EventOverride> {
  const merged = { ...state.eventOverrides[eventId], ...patch };
  const settled = settleOverride(
    merged,
    state.events.find((e) => e.id === eventId) ?? PHANTOM_EVENT,
  );
  const next = { ...state.eventOverrides };
  if (settled) next[eventId] = settled;
  else delete next[eventId];
  return capOverrides(next);
}

/** Re-settle every pending assertion against the wire truth that just arrived.
 * `lookup` returns the committed event for an id, or `undefined` when the new
 * truth does not carry it at all (a resync that dropped it) — in which case the
 * whole entry goes. */
function settleOverrides(
  overrides: Record<number, EventOverride>,
  lookup: (id: number) => EventItem | undefined,
): Record<number, EventOverride> {
  const next: Record<number, EventOverride> = {};
  for (const [key, override] of Object.entries(overrides)) {
    const id = Number(key);
    const settled = settleOverride(override, lookup(id));
    if (settled) next[id] = settled;
  }
  return next;
}

/** Upsert a view by `instance_id`, preserving list position for a known id so a
 * re-`view_upsert` (update in place) re-renders that slot rather than reordering
 * it. Solid's `reconcile` (keyed by instance_id) then re-renders only the DOM
 * bound to the changed spec. */
function upsertView(views: ViewInstance[], view: ViewInstance): ViewInstance[] {
  const idx = views.findIndex((v) => v.instance_id === view.instance_id);
  if (idx === -1) return [...views, view];
  const next = views.slice();
  next[idx] = view;
  return next;
}

/** Upsert a process by id, preserving list position for a known id (the
 * newest-activity-first ordering is applied by the selector, not stored). */
function upsertProcess(processes: ProcessInfo[], process: ProcessInfo): ProcessInfo[] {
  const idx = processes.findIndex((p) => p.id === process.id);
  if (idx === -1) return [...processes, process];
  const next = processes.slice();
  next[idx] = process;
  return next;
}

/** Insert a turn timeline event keyed by `seq` (idempotent on redelivery),
 * keeping the list sorted by seq. Out-of-order arrivals sort into place; gaps
 * are left as-is (the fold renders what is present, never buffering). */
function upsertTurnEvent(events: TimelineEvent[], event: TimelineEvent): TimelineEvent[] {
  const withoutDup = events.filter((e) => e.seq !== event.seq);
  return [...withoutDup, event].sort((a, b) => a.seq - b.seq);
}

/** Streamed deltas can land the turn's final prose chunk with different
 * trailing whitespace than the committed body (or vice versa), so a plain
 * `===` would miss near-duplicates; tolerate either string being a prefix of
 * the other, on top of an exact trimmed match. */
function proseMatchesBody(proseText: string, body: string): boolean {
  const p = proseText.trim();
  const b = body.trim();
  return p === b || b.startsWith(p) || p.startsWith(b);
}

/** Drop the turn timeline's trailing prose block when it just restates the
 * committed message body — the turn's final prose IS the committed body, so
 * showing it again as the last row of "turn details" is pure duplication.
 * Only the trailing block is a candidate: intermediate prose (thinking out
 * loud along the way) is the whole point of the expander and is left alone. */
function trimTrailingProse(events: TimelineEvent[], body: string): TimelineEvent[] {
  const last = events[events.length - 1];
  if (!last || last.event.kind !== "prose" || !proseMatchesBody(last.event.text, body)) {
    return events;
  }
  return events.slice(0, -1);
}

/** Freeze the just-finished turn's timeline onto the committing message id,
 * dropping the oldest retained turn once over the session cap.
 *
 * `details` may be a live reactive store proxy, so a plain
 * `{ ...details, [msgId]: events }` copies the *other* entries' array
 * references verbatim rather than cloning their contents. Feeding one of
 * those live references back into a store write — even nested inside an
 * otherwise-fresh wrapper object can make a same-length array replacement
 * land empty. Every entry gets a fresh array here for that reason. */
function retainTurnDetails(
  details: Record<number, TimelineEvent[]>,
  msgId: number,
  events: TimelineEvent[],
): Record<number, TimelineEvent[]> {
  const next: Record<number, TimelineEvent[]> = {};
  for (const [key, existing] of Object.entries(details)) {
    next[Number(key)] = [...existing];
  }
  next[msgId] = [...events];
  const ids = Object.keys(next).map(Number);
  if (ids.length > TURN_DETAILS_LIMIT) {
    for (const id of ids.sort((a, b) => a - b).slice(0, ids.length - TURN_DETAILS_LIMIT)) {
      delete next[id];
    }
  }
  return next;
}

function setUpload(uploads: Upload[], clientId: string, patch: Partial<Upload>): Upload[] {
  return uploads.map((u) => (u.clientId === clientId ? { ...u, ...patch } : u));
}

/** Reconcile an incoming owner-authored `msg` against the oldest still-pending
 * optimistic send whose body matches (protocol.md: "replaces optimistic entry
 * with the first msg whose author=owner and body matches"). The optimistic
 * entry's `clientId`/`mode` are preserved onto the reconciled message so a
 * next_turn bubble stays cancellable (cancel_queued) after the host echo. */
function reconcileOrAppend(state: AppState, message: DisplayMessage): AppState {
  if (state.messages.some((m) => !m.pending && m.id === message.id)) {
    // Already known (e.g. re-delivered); nothing to do.
    return state;
  }

  if (message.author === "owner") {
    const pendingIdx = state.messages.findIndex((m) => m.pending && m.body === message.body);
    if (pendingIdx !== -1) {
      const reconciled = state.messages[pendingIdx];
      const nextMessages = state.messages.slice();
      // Preserve clientId/mode only for next_turn sends, which stay cancellable
      // (cancel_queued) after the echo. Plain sends reconcile to the bare host
      // message so nothing lingers on them.
      nextMessages[pendingIdx] =
        reconciled.mode === "next_turn"
          ? { ...message, clientId: reconciled.clientId, mode: reconciled.mode }
          : { ...message };
      return {
        ...state,
        messages: nextMessages,
        pendingSends: state.pendingSends.filter((p) => p.clientId !== reconciled.clientId),
      };
    }
  }

  const grown = [...state.messages, message];
  const messages = capMessages(grown);
  return {
    ...state,
    messages,
    hasEarlierMessages: state.hasEarlierMessages || messages.length < grown.length,
  };
}

/** A live row beyond an intentionally older bounded range is acknowledged but
 * not spliced across the gap. Owner echoes still reconcile pending sends; the
 * latest-page jump reloads this row from the Host. */
function reconcileBeyondLoadedRange(state: AppState, message: DisplayMessage): AppState {
  if (message.author !== "owner") return state;
  const pending = state.messages.find(
    (candidate) => candidate.pending && candidate.body === message.body,
  );
  if (!pending) return state;
  return {
    ...state,
    messages: state.messages.filter((candidate) => candidate !== pending),
    pendingSends: state.pendingSends.filter((send) => send.clientId !== pending.clientId),
  };
}

export function reduce(state: AppState, action: Action): AppState {
  switch (action.type) {
    case "hello_ok": {
      const { latest_msg_id } = action.payload;
      // Never re-admit a tombstoned (cancelled) id from replay.
      const snapshot = action.payload.messages.filter((m) => !state.removedIds.includes(m.id));
      // The snapshot is AUTHORITATIVE for the id range it covers (>= its minimum
      // id). A C7 full resync (host `build_snapshot` with no cursor, sent when a
      // client's broadcast receiver lags) starts at the true minimum, so it
      // supersedes the whole range — a message removed during the lag gap is
      // dropped rather than left as residue, making a second hello_ok an
      // idempotent full-state replace. A handshake replay is cursor-based (only
      // ids > the client's last_seen), so local history BELOW the snapshot range
      // is preserved instead of wiped. One rule handles both (the client can't
      // tell them apart on the wire).
      let snapshotMin = Number.POSITIVE_INFINITY;
      for (const m of snapshot) if (m.id < snapshotMin) snapshotMin = m.id;
      // Ids already held locally (committed) — used only to decide which replayed
      // messages are GENUINELY new (for the pending reconciliation below), kept
      // separate from the range-merge so an already-known message can't be
      // mistaken for a fresh echo of an optimistic send that shares its body.
      const localIds = new Set<number>();
      for (const m of state.messages) if (!m.pending) localIds.add(m.id);
      // Range-authoritative merge: keep local history strictly BELOW the
      // snapshot's covered range; the snapshot owns everything from its minimum
      // id up (so a full resync drops residue, a partial replay preserves older
      // local rows).
      const known = new Map<number, DisplayMessage>();
      for (const m of state.messages) {
        if (!state.hasLaterMessages && !m.pending && m.id < snapshotMin) known.set(m.id, m);
      }
      const newlyReplayed = snapshot.filter((m) => !localIds.has(m.id));
      for (const m of snapshot) known.set(m.id, m);
      const merged = Array.from(known.values()).sort((a, b) => a.id - b.id);

      // Reconcile optimistic entries against the replay: a send may have
      // reached the host right before the disconnect, in which case its echo
      // arrives here as a replayed owner message instead of a live `msg`.
      let pending = state.messages.filter((m) => m.pending);
      let pendingSends = state.pendingSends;
      for (const m of newlyReplayed) {
        if (m.author !== "owner") continue;
        const idx = pending.findIndex((p) => p.body === m.body);
        if (idx === -1) continue;
        const reconciled = pending[idx];
        pending = pending.filter((_, i) => i !== idx);
        pendingSends = pendingSends.filter((p) => p.clientId !== reconciled.clientId);
      }

      const cappedMessages = capMessages([...merged, ...pending]);
      const oldestCommitted = cappedMessages.find((message) => !message.pending)?.id;
      return {
        ...state,
        messages: cappedMessages,
        hasEarlierMessages: oldestCommitted !== undefined && oldestCommitted > 1,
        hasLaterMessages: false,
        // Task snapshot: the compatibility wire's Event set is
        // authoritative on reconnect — a full replace (an event resolved while
        // offline is reflected). Defensive default so a malformed
        // frame can't white-screen the app.
        events: action.payload.events ?? [],
        // ONE resync rule for the whole optimistic layer: every assertion the
        // snapshot has caught up with is dropped, and an event the snapshot no
        // longer carries drops its entry outright. What survives is exactly what
        // the host has not committed yet — so a decide/archive/read/snooze made
        // just before the disconnect still holds, and nothing stale lingers.
        eventOverrides: settleOverrides(state.eventOverrides, (id) =>
          (action.payload.events ?? []).find((e) => e.id === id),
        ),
        lastSeenMsgId: latest_msg_id,
        // Keep the last reported host version if this frame (or an older host)
        // omits it, so a resync never regresses About to "Not reported".
        hostVersion: action.payload.host_version ?? state.hostVersion,
        // Model configuration is authoritative on (re)connect: seed both from
        // the frame, defaulting to null when a field is absent (older hosts) so
        // the Settings/header controls hide rather than render stale data.
        model: action.payload.model ?? null,
        subagentModels: action.payload.subagent_models ?? null,
        pendingSends,
        // Fresh sync boundary: seed processes; the live turn timeline (ephemeral,
        // never replayed) does not survive a resync. Retained turn details for
        // already-shown messages are kept (session memory, orthogonal to sync).
        processes: action.payload.processes ?? [],
        turnEvents: [],
        lastTurnEvents: [],
        // Generative-UI tier: the snapshot's view set is authoritative on
        // reconnect — a full replace (a view cleared while offline is gone).
        // Defensive default like processes so a malformed frame can't
        // white-screen the app.
        views: action.payload.views ?? [],
      };
    }

    case "messages_page": {
      const page = action.payload.messages.filter(
        (message) => !state.removedIds.includes(message.id),
      );
      const pending = state.messages.filter((message) => message.pending);

      if (action.placement === "latest") {
        const known = new Map<number, DisplayMessage>();
        for (const message of page) known.set(message.id, message);
        const committed = Array.from(known.values()).sort((a, b) => a.id - b.id);
        return {
          ...state,
          messages: capMessages([...committed, ...pending]),
          hasEarlierMessages: action.payload.has_more,
          hasLaterMessages: false,
        };
      }

      const known = new Map<number, DisplayMessage>();
      for (const message of state.messages) {
        if (!message.pending) known.set(message.id, message);
      }
      for (const message of page) known.set(message.id, message);
      const committed = Array.from(known.values()).sort((a, b) => a.id - b.id);
      const merged = [...committed, ...pending];
      const messages = capMessagesKeepingOldest(merged);
      return {
        ...state,
        messages,
        hasEarlierMessages: action.payload.has_more,
        hasLaterMessages: state.hasLaterMessages || messages.length < merged.length,
      };
    }

    case "msg": {
      const message = action.payload.message;
      // A tombstoned id (cancelled queued message) must never re-materialize,
      // even if its echo arrives after the msg_removed that killed it.
      if (state.removedIds.includes(message.id)) return state;
      const next = state.hasLaterMessages
        ? reconcileBeyondLoadedRange(state, message)
        : reconcileOrAppend(state, message);

      // A committed agent message ends the turn: freeze its live timeline into
      // session memory keyed to this message (the "turn details" affordance),
      // then clear the ephemeral live buffer. The timeline is normally still
      // live, but the Host's idle boundary usually beats the message it
      // belongs to onto the wire (see `lastTurnEvents`), in which case the
      // just-ended turn's events are parked there instead — either way the
      // commit freezes them. The timeline's trailing prose block is dropped
      // first when it just restates the committed body (see trimTrailingProse)
      // — otherwise the expander's last row would be a verbatim repeat of the
      // message everyone can already read. Only stash a non-empty (post-trim)
      // timeline.
      const commits = message.author === "agent";
      const liveEvents = state.turnEvents.length > 0 ? state.turnEvents : state.lastTurnEvents;
      const frozenEvents = commits ? trimTrailingProse(liveEvents, message.body) : state.turnEvents;
      const stash = commits && frozenEvents.length > 0;
      return {
        ...next,
        lastSeenMsgId:
          state.lastSeenMsgId === null ? message.id : Math.max(state.lastSeenMsgId, message.id),
        turnEvents: commits ? [] : next.turnEvents,
        lastTurnEvents: commits ? [] : next.lastTurnEvents,
        turnDetails: stash
          ? retainTurnDetails(state.turnDetails, message.id, frozenEvents)
          : next.turnDetails,
      };
    }

    case "msg_removed": {
      // Tombstone for a cancelled queued message: drop the bubble and any
      // still-pending optimistic entry / pendingSend that carried it.
      const removed = state.messages.find((m) => m.id === action.id);
      const removedClientId = removed?.clientId;
      const removedIds = state.removedIds.includes(action.id)
        ? state.removedIds
        : [...state.removedIds, action.id].slice(-200);
      return {
        ...state,
        messages: state.messages.filter((m) => m.id !== action.id),
        pendingSends: removedClientId
          ? state.pendingSends.filter((p) => p.clientId !== removedClientId)
          : state.pendingSends,
        removedIds,
      };
    }

    case "agent_activity":
      return {
        ...state,
        agentActivity: {
          state: action.payload.state,
          text: action.payload.text,
        },
        // Turn boundary: idle ends the live timeline, so it stops rendering
        // under the thinking marker — but the committing agent message lands
        // AFTER this frame (the Host publishes idle from the observation
        // bridge and the message from the turn pump), so the events are parked
        // in `lastTurnEvents` for that commit to freeze rather than dropped.
        // A cancelled turn goes idle with no commit to follow; its parked
        // timeline is simply discarded when the next turn starts.
        turnEvents: action.payload.state === "idle" ? [] : state.turnEvents,
        lastTurnEvents:
          action.payload.state === "idle" && state.turnEvents.length > 0
            ? state.turnEvents
            : state.lastTurnEvents,
      };

    case "process_upsert":
      return {
        ...state,
        processes: upsertProcess(state.processes, action.payload.process),
      };

    case "turn_event":
      return {
        ...state,
        // A turn event after an idle boundary belongs to a NEW turn: whatever
        // the previous turn parked is now stale (its commit either landed or
        // never will) and must not attach to this turn's message.
        lastTurnEvents: [],
        turnEvents: upsertTurnEvent(state.turnEvents, {
          seq: action.payload.seq,
          event: action.payload.event,
          at: Date.now(),
        }),
      };

    // ---- Task collection (typed Event wire frames) ----

    case "event_upsert": {
      const event = action.payload.event;
      const pending = state.eventOverrides[event.id];
      let eventOverrides = state.eventOverrides;
      if (pending) {
        // The same settle rule as a resync, applied to this one id: a committed
        // value supersedes the assertion that predicted it, and an assertion the
        // echo has NOT caught up with survives — an interleaved upsert (a read
        // flip racing the archive echo, say) must not flicker the card back.
        const settled = settleOverride(pending, event);
        eventOverrides = { ...state.eventOverrides };
        if (settled) eventOverrides[event.id] = settled;
        else delete eventOverrides[event.id];
      }
      return { ...state, events: upsertEvent(state.events, event), eventOverrides };
    }

    // ---- The optimistic layer: six gestures, one record ----
    //
    // Each records the value it ASSERTS about the event; `assertOverride`
    // settles it against the wire truth on the spot, so a gesture that merely
    // restates what the host already says leaves nothing behind. `events` is
    // never patched in place — the wire truth stays intact to reconcile against.

    case "event_decide_local":
      // Decide: the card renders/counts as decided at once while `event_action`
      // is sent and a ~5s Undo window offers recovery.
      return { ...state, eventOverrides: assertOverride(state, action.eventId, { decided: true }) };

    case "event_undecide_local":
      // Undo / Reopen: assert the event is open again (dropping the assertion
      // outright when the wire already has it open).
      return { ...state, eventOverrides: assertOverride(state, action.eventId, { decided: false }) };

    case "event_read_local":
      // Awareness auto-read as the scroller passes it.
      return { ...state, eventOverrides: assertOverride(state, action.eventId, { read: true }) };

    case "event_archive_local":
      // Archive: the event leaves the resting queue at once while
      // `event_action{archive}` is sent.
      return { ...state, eventOverrides: assertOverride(state, action.eventId, { archived: true }) };

    case "event_unarchive_local":
      // Unarchive: assert not-archived, which returns the row to the resting
      // queue whichever layer archived it — the wire flag or a pending assertion.
      return {
        ...state,
        eventOverrides: assertOverride(state, action.eventId, { archived: false }),
      };

    case "event_snooze_local":
      // Durable snooze (Wave-3): the card leaves Active at once while
      // `event_action{snooze,{until}}` is sent. The host echo carries the same
      // instant (and later clears it at the return moment) and settles this.
      return {
        ...state,
        eventOverrides: assertOverride(state, action.eventId, { snoozedUntil: action.until }),
      };

    case "event_unsnooze_local":
      // Un-snooze: assert no return instant, so the event is back in Active now.
      return {
        ...state,
        eventOverrides: assertOverride(state, action.eventId, { snoozedUntil: null }),
      };

    case "send_local": {
      const { localId, clientId, body, ref, ts, attachments, mode, mentions } = action;
      const blobs = attachments ?? [];
      const sendMode = mode ?? "send";
      const mentionIds = mentions ?? [];
      // Negative synthetic ids keep optimistic entries clearly out of the
      // host's id space; they are never sorted against real ids, only appended
      // and later replaced in place once reconciled.
      const localMessage: DisplayMessage = {
        id: localId,
        author: "owner",
        body,
        ref,
        ts,
        attachments: blobs,
        pending: true,
        clientId,
        mode: sendMode,
      };
      // Total: a plain message carries `[]` / "send" / `[]` as real values. The
      // wire's one omission (empty `mentions`) is applied by `sendMessageFrame`,
      // not encoded here.
      const pendingSend: PendingSend = {
        clientId,
        body,
        ref,
        attachments: blobs.map((b) => b.id),
        mode: sendMode,
        mentions: mentionIds,
      };
      const grown = [...state.messages, localMessage];
      return {
        ...state,
        messages: state.hasLaterMessages
          ? capMessagesKeepingOldest(grown)
          : capMessages(grown),
        pendingSends: [...state.pendingSends, pendingSend],
      };
    }

    case "send_failed":
      return {
        ...state,
        messages: state.messages.map((m) =>
          m.pending && m.clientId === action.clientId ? { ...m, failed: true } : m,
        ),
      };

    case "send_retry":
      return {
        ...state,
        messages: state.messages.map((m) =>
          m.pending && m.clientId === action.clientId ? { ...m, failed: false } : m,
        ),
      };

    case "upload_start":
      return {
        ...state,
        uploads: [
          ...state.uploads.filter((u) => u.clientId !== action.clientId),
          {
            clientId: action.clientId,
            name: action.name,
            size: action.size,
            mime: action.mime,
            state: "uploading",
          },
        ],
      };

    case "blob_ok":
      return {
        ...state,
        uploads: setUpload(state.uploads, action.clientId, {
          state: "done",
          blobId: action.blob.id,
        }),
      };

    case "upload_error":
      return {
        ...state,
        uploads: setUpload(state.uploads, action.clientId, { state: "error" }),
      };

    case "upload_retry":
      return {
        ...state,
        uploads: setUpload(state.uploads, action.clientId, { state: "uploading" }),
      };

    case "upload_remove":
      return {
        ...state,
        uploads: state.uploads.filter((u) => u.clientId !== action.clientId),
      };

    case "uploads_clear":
      return { ...state, uploads: [] };

    case "connection_status":
      return { ...state, connection: action.status };

    case "view_upsert": {
      const { instance_id, placement, spec } = action.payload;
      return {
        ...state,
        views: upsertView(state.views, { instance_id, placement, spec }),
      };
    }

    case "view_removed":
      return {
        ...state,
        views: state.views.filter((v) => v.instance_id !== action.payload.instance_id),
      };

    case "model_changed": {
      // Patch only `current`; the `available` list is unchanged. Ignore
      // gracefully if no snapshot has been seeded yet (a stray broadcast on an
      // older host that never sent `hello_ok.model` must not synthesize one).
      if (!state.model) return state;
      return {
        ...state,
        model: { ...state.model, current: action.current },
      };
    }

    case "subagent_models_changed":
      // Replace the catalog wholesale with the new one.
      return { ...state, subagentModels: action.catalog };

    default:
      return state;
  }
}
