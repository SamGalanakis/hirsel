// Pure reducer over the client's protocol-facing state. Kept free of any
// WebSocket concerns so it can be unit tested directly (see reducer.test.ts)
// and reused verbatim by the Solid store.
import type { ChatMessage, EventItem, Ping, ProcessInfo, ViewInstance } from "../protocol";
import type {
  Action,
  AppState,
  DisplayMessage,
  PendingSend,
  SideChatState,
  TimelineEvent,
  Upload,
} from "./types";

/** Cap on retained finished-turn timelines (session memory for "turn details").
 * A live chat session has few turns; this only guards a pathological long run. */
const TURN_DETAILS_LIMIT = 50;

/** Hard cap on the in-memory chat history (D10). An always-open PWA would
 * otherwise grow `messages` without bound across a multi-day session — every
 * append is O(n) to render and the unseen scan is O(n) per scroll. We keep the
 * most-recent slice; older rows fall out of memory (the host still holds the
 * canonical history, replayed from `last_seen` on reconnect). Comfortably larger
 * than the render window (see ChatView) so "load older" has headroom. Oldest are
 * dropped from the FRONT, so optimistic sends (always newest, at the tail) and
 * the reconciliation that matches them are never disturbed. */
export const MESSAGES_CAP = 600;

/** Keep only the newest `MESSAGES_CAP` messages, dropping the oldest. A no-op
 * (same reference) under the cap so the common case allocates nothing. */
function capMessages(messages: DisplayMessage[]): DisplayMessage[] {
  return messages.length > MESSAGES_CAP ? messages.slice(messages.length - MESSAGES_CAP) : messages;
}

function upsertPing(pings: Ping[], ping: Ping): Ping[] {
  const idx = pings.findIndex((existing) => existing.id === ping.id);
  if (idx === -1) return [...pings, ping];
  const next = pings.slice();
  next[idx] = ping;
  return next;
}

/** Upsert an Event by id, preserving list position for a known id (the queue
 * ordering is applied by the selector, not stored) — same shape as upsertPing. */
function upsertEvent(events: EventItem[], event: EventItem): EventItem[] {
  const idx = events.findIndex((e) => e.id === event.id);
  if (idx === -1) return [...events, event];
  const next = events.slice();
  next[idx] = event;
  return next;
}

/** v2.1 (ADR-0009): the Owner replying to a Ping's Anchor resolves it to
 * `done`. The host does this authoritatively (and broadcasts a ping_upsert),
 * but the client flips optimistically the moment the reply is sent — exactly
 * like read-state — so a replied Ping leaves the open list immediately instead
 * of lingering until the echo. Idempotent and a no-op when `ref` matches no
 * open Ping; the host upsert reconciles the truth. */
function resolveOpenPingByAnchor(pings: Ping[], ref: number | null): Ping[] {
  if (ref === null) return pings;
  const idx = pings.findIndex((p) => p.status === "open" && p.anchor === ref);
  if (idx === -1) return pings;
  const next = pings.slice();
  next[idx] = { ...next[idx], status: "done" };
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
 * `turnDetails` has the identical `Record<key, TimelineEvent[]>` shape as
 * `sideChats` and shares its latent hazard (see cloneSideChat/setSideChat's
 * notes): `details` may be a live reactive store proxy, so a plain
 * `{ ...details, [msgId]: events }` copies the *other* entries' array
 * references verbatim rather than cloning their contents. Feeding one of
 * those live references back into a store write — even nested inside an
 * otherwise-fresh wrapper object — is what caused sideChats' same-length
 * array replacement to intermittently land empty. Every entry (the new one
 * and every carried-over one) gets a fresh array here for the same reason. */
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

  return { ...state, messages: capMessages([...state.messages, message]) };
}

/** Same idea as `reconcileOrAppend`, scoped to one side chat's own message
 * list. Side sends are text-only (no attachments/mode), so this is a smaller
 * shape: it just returns the next list plus the reconciled clientId (if any)
 * so the caller can drop the matching `pendingSideSends` entry. */
function reconcileSideMessages(
  messages: DisplayMessage[],
  message: DisplayMessage,
): { messages: DisplayMessage[]; reconciledClientId: string | null } {
  if (messages.some((m) => !m.pending && m.id === message.id)) {
    return { messages, reconciledClientId: null };
  }
  if (message.author === "owner") {
    const idx = messages.findIndex((m) => m.pending && m.body === message.body);
    if (idx !== -1) {
      const reconciled = messages[idx];
      const next = messages.slice();
      next[idx] = { ...message };
      return { messages: next, reconciledClientId: reconciled.clientId ?? null };
    }
  }
  return { messages: [...messages, message], reconciledClientId: null };
}

/** Shallow-clone a SideChatState, including FRESH references for its two
 * array fields. `state` (the reducer's input) may be a live reactive store
 * proxy (see store.ts's `appSnapshot`): spreading it copies key/value pairs
 * but not nested array *contents*, so a carried-over `messages`/`turnEvents`
 * stays the exact same tracked array reference. Feeding that same reference
 * back into a store write — even nested inside an otherwise-fresh wrapper
 * object — confused Solid's merge on the *next* write to that path (traced
 * to an empty `messages` after a resume's `side_chat_open`). Spread this
 * before layering further overrides, everywhere an existing/current
 * SideChatState is carried into a new one. */
function cloneSideChat(sideChat: SideChatState): SideChatState {
  return { ...sideChat, messages: [...sideChat.messages], turnEvents: [...sideChat.turnEvents] };
}

/** Set (insert or replace) one entry in the sideChats map, cloning every
 * OTHER entry through too rather than letting it ride along as a live store
 * reference embedded in the otherwise-fresh returned object — the same
 * hazard `cloneSideChat` guards against, just at the map level instead of a
 * single entry. `value` should already be a fresh SideChatState (built via
 * `cloneSideChat(existing)` plus overrides, or `freshSideChat`). Side chat
 * counts are small (a handful at most, per the critique's "multiple side
 * chats" case), so cloning every entry on every update is cheap. */
function setSideChat(
  sideChats: AppState["sideChats"],
  sc: string,
  value: SideChatState,
): AppState["sideChats"] {
  const next: AppState["sideChats"] = {};
  for (const [key, existing] of Object.entries(sideChats)) {
    next[key] = key === sc ? value : cloneSideChat(existing);
  }
  if (!(sc in next)) next[sc] = value;
  return next;
}

/** A fresh SideChatState for a side chat the client has just learned is live
 * (opened, resumed, or seeded from `hello_ok.side_chats`). */
function freshSideChat(sc: string, pingId: number, messages: ChatMessage[]): SideChatState {
  return {
    sc,
    pingId,
    messages,
    agentActivity: { state: "idle", text: null },
    turnEvents: [],
    drafting: false,
    draft: null,
    confirming: false,
    discarding: false,
    pingResolved: false,
    ended: false,
  };
}

export function reduce(state: AppState, action: Action): AppState {
  switch (action.type) {
    case "hello_ok": {
      const { latest_msg_id, pings } = action.payload;
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
        if (!m.pending && m.id < snapshotMin) known.set(m.id, m);
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

      // v2.0: reconcile live side chats against hello_ok.side_chats. A sc
      // still listed survives untouched (its hydrated state, if any, is kept
      // as-is — the resume flow re-fetches the transcript via open_side_chat,
      // which is idempotent). A sc that was hydrated but has now vanished
      // either resolved while offline (this client asked for the close, so
      // the outcome is already known — drop it silently) or was closed
      // elsewhere/reaped (mark it `ended` so a currently-open sheet shows the
      // graceful terminal state instead of just disappearing).
      const sideChatRefs = action.payload.side_chats ?? [];
      const liveScs = new Set(sideChatRefs.map((r) => r.sc));
      const sideChats: AppState["sideChats"] = {};
      for (const [sc, sideChat] of Object.entries(state.sideChats)) {
        if (liveScs.has(sc)) {
          sideChats[sc] = cloneSideChat(sideChat);
        } else if (!sideChat.confirming && !sideChat.discarding) {
          sideChats[sc] = { ...cloneSideChat(sideChat), ended: true };
        }
        // else: resolved while offline as expected — drop.
      }

      return {
        ...state,
        messages: capMessages([...merged, ...pending]),
        // Defensive, matching processes/side_chats below: a malformed frame
        // that omits `pings` must not white-screen the whole app.
        pings: pings ?? [],
        // Typed event queue (ADR-0012): the snapshot's event set is
        // authoritative on reconnect — a full replace (an event resolved while
        // offline is reflected). Defensive default like pings so a malformed
        // frame can't white-screen the app.
        events: action.payload.events ?? [],
        // Drop optimistic decide overrides for events the snapshot no longer
        // carries, so a resync leaves no stale override residue (mirrors the
        // unreadOverrides recompute).
        eventDecideOverrides: state.eventDecideOverrides.filter((id) =>
          (action.payload.events ?? []).some((e) => e.id === id),
        ),
        // Same recompute for the optimistic archive layer: an id the snapshot
        // no longer carries (or already carries as archived — the committed
        // truth) leaves no stale override residue behind.
        eventArchiveOverrides: state.eventArchiveOverrides.filter((id) =>
          (action.payload.events ?? []).some((e) => e.id === id && e.archived !== true),
        ),
        // Signal the wholesale event-set swap to the queue scroller so it
        // re-anchors its viewport and drops per-session bookkeeping (see
        // AppState.eventsSnapshotSeq).
        eventsSnapshotSeq: state.eventsSnapshotSeq + 1,
        // Recompute unread against the authoritative ping set: drop client-only
        // "mark unread" overrides for pings the snapshot no longer contains, so a
        // resync leaves no stale unread residue.
        unreadOverrides: state.unreadOverrides.filter((id) =>
          (pings ?? []).some((p) => p.id === id),
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
        sideChatRefs,
        sideChats,
        // Generative-UI tier: the snapshot's view set is authoritative on
        // reconnect — a full replace (a view cleared while offline is gone).
        // Defensive default like pings/processes so a malformed frame can't
        // white-screen the app.
        views: action.payload.views ?? [],
      };
    }

    case "msg": {
      const message = action.payload.message;
      // A tombstoned id (cancelled queued message) must never re-materialize,
      // even if its echo arrives after the msg_removed that killed it.
      if (state.removedIds.includes(message.id)) return state;
      const next = reconcileOrAppend(state, message);

      // v2.0 provenance (client-derived; no wire marker): a plain owner reply
      // whose ref matches an anchor a confirm_conclusion just targeted is the
      // conclusion landing in main chat. Tag it for the footer chip and fire
      // the one-shot "land and highlight" signal ChatView consumes.
      let awaitingConclusions = next.awaitingConclusions;
      let conclusionChips = next.conclusionChips;
      let lastConclusion = next.lastConclusion;
      if (message.author === "owner" && message.ref !== null && awaitingConclusions[message.ref]) {
        const sc = awaitingConclusions[message.ref];
        const rest = { ...awaitingConclusions };
        delete rest[message.ref];
        awaitingConclusions = rest;
        conclusionChips = [...conclusionChips, message.id].slice(-500);
        lastConclusion = { sc, messageId: message.id };
      }

      // A committed agent message ends the turn: freeze its live timeline into
      // session memory keyed to this message (the "turn details" affordance),
      // then clear the ephemeral live buffer. The timeline's trailing prose
      // block is dropped first when it just restates the committed body (see
      // trimTrailingProse) — otherwise the expander's last row would be a
      // verbatim repeat of the message everyone can already read. Only stash
      // a non-empty (post-trim) timeline.
      const commits = message.author === "agent";
      const frozenEvents = commits ? trimTrailingProse(state.turnEvents, message.body) : state.turnEvents;
      const stash = commits && frozenEvents.length > 0;
      return {
        ...next,
        awaitingConclusions,
        conclusionChips,
        lastConclusion,
        lastSeenMsgId:
          state.lastSeenMsgId === null ? message.id : Math.max(state.lastSeenMsgId, message.id),
        turnEvents: commits ? [] : next.turnEvents,
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
        // Turn boundary: idle clears the live timeline (a cancelled turn may go
        // idle without a committed message; its partial timeline is dropped).
        turnEvents: action.payload.state === "idle" ? [] : state.turnEvents,
      };

    case "process_upsert":
      return {
        ...state,
        processes: upsertProcess(state.processes, action.payload.process),
      };

    case "turn_event":
      return {
        ...state,
        turnEvents: upsertTurnEvent(state.turnEvents, {
          seq: action.payload.seq,
          event: action.payload.event,
          at: Date.now(),
        }),
      };

    case "ping_upsert": {
      const ping = action.payload.ping;
      // Edge case (critique, binding): the Agent can resolve a Ping while its
      // side chat is still open. Don't kill the sheet — flag a non-blocking
      // banner; Conclude/Discard both remain available. Fully derivable here,
      // so no separate action/dispatch is needed for it.
      let sideChats = state.sideChats;
      if (ping.status !== "open") {
        const hasResolvable = Object.values(state.sideChats).some(
          (sc) => sc.pingId === ping.id && !sc.pingResolved,
        );
        // Guarded so the common case (no matching live side chat) leaves
        // `sideChats` as the untouched live reference — cheap, and safe,
        // since nothing rebuilds a fresh wrapper around it in that case. Once
        // something IS changing, every entry (not just the matching one) is
        // cloned through, not just aliased into the fresh map (see
        // cloneSideChat's note on why a live reference can't ride along).
        if (hasResolvable) {
          const next: AppState["sideChats"] = {};
          for (const [sc, sideChat] of Object.entries(state.sideChats)) {
            next[sc] =
              sideChat.pingId === ping.id && !sideChat.pingResolved
                ? { ...cloneSideChat(sideChat), pingResolved: true }
                : cloneSideChat(sideChat);
          }
          sideChats = next;
        }
      }
      return {
        ...state,
        pings: upsertPing(state.pings, ping),
        // A committed resolve (host `done` ping_upsert) supersedes any pending
        // optimistic Mark-done override for this id — prune it so the wire truth
        // and the optimistic layer never disagree once the send has landed.
        resolveOverrides:
          ping.status !== "open"
            ? state.resolveOverrides.filter((id) => id !== ping.id)
            : state.resolveOverrides,
        sideChats,
      };
    }

    case "read_local": {
      // Optimistic email-like "seen" flip: set read=true locally (the host's
      // ping_upsert reconciles it) and drop any manual unread override, since
      // reading always wins over a prior "Mark unread".
      const pings = state.pings.map((p) =>
        p.id === action.pingId ? { ...p, read: true } : p,
      );
      return {
        ...state,
        pings,
        unreadOverrides: state.unreadOverrides.filter((id) => id !== action.pingId),
      };
    }

    case "mark_unread_local": {
      // Client-only override (no wire unread op): record the id so the Ping is
      // rendered/counted as unread even though the wire `read` flag stays true.
      const unreadOverrides = state.unreadOverrides.includes(action.pingId)
        ? state.unreadOverrides
        : [...state.unreadOverrides, action.pingId].slice(-200);
      return { ...state, unreadOverrides };
    }

    case "resolve_local": {
      // Optimistic "Marked done" flip (mirrors `mark_unread_local`): record the
      // id so the Ping renders/counts as resolved at once, while a 5s Undo
      // window (lib/resolve-undo) debounces the actual `resolve_ping` send.
      const resolveOverrides = state.resolveOverrides.includes(action.pingId)
        ? state.resolveOverrides
        : [...state.resolveOverrides, action.pingId].slice(-200);
      return { ...state, resolveOverrides };
    }

    case "unresolve_local": {
      // Undo tapped inside the window (or the send superseded): drop the
      // optimistic override so the Ping returns to its wire status.
      return {
        ...state,
        resolveOverrides: state.resolveOverrides.filter((id) => id !== action.pingId),
      };
    }

    // ---- Typed event queue (ADR-0012), generalizing the ping slice above ----

    case "event_upsert": {
      const event = action.payload.event;
      return {
        ...state,
        events: upsertEvent(state.events, event),
        // A committed resolve (host `done` event_upsert) supersedes any pending
        // optimistic decide override for this id — prune it so the wire truth and
        // the optimistic layer never disagree once the action has landed (twin of
        // the ping_upsert resolveOverrides prune).
        eventDecideOverrides:
          event.status !== "open"
            ? state.eventDecideOverrides.filter((id) => id !== event.id)
            : state.eventDecideOverrides,
        // The archive twin: a committed archived event_upsert supersedes any
        // pending optimistic archive override for this id. Only the COMMITTED
        // archived truth prunes — an unrelated interleaved upsert (still
        // archived=false) must not drop the override and flicker the card back
        // before the archive echo lands (same guard as the decide prune above).
        eventArchiveOverrides:
          event.archived === true
            ? state.eventArchiveOverrides.filter((id) => id !== event.id)
            : state.eventArchiveOverrides,
      };
    }

    case "event_decide_local": {
      // Optimistic decide flip (mirrors `resolve_local`): record the id so the
      // event renders/counts as decided at once, while `event_action` is sent to
      // the host and a ~5s Undo window offers recovery.
      const eventDecideOverrides = state.eventDecideOverrides.includes(action.eventId)
        ? state.eventDecideOverrides
        : [...state.eventDecideOverrides, action.eventId].slice(-200);
      return { ...state, eventDecideOverrides };
    }

    case "event_undecide_local": {
      // Undo (mirrors `unresolve_local`): drop the optimistic override so the
      // event returns to its wire status.
      return {
        ...state,
        eventDecideOverrides: state.eventDecideOverrides.filter((id) => id !== action.eventId),
      };
    }

    case "event_read_local": {
      // Awareness auto-read as the scroller passes it (mirrors `read_local`):
      // flip read=true locally; the host's event_upsert reconciles the truth.
      const events = state.events.map((e) =>
        e.id === action.eventId ? { ...e, read: true } : e,
      );
      return { ...state, events };
    }

    case "event_archive_local": {
      // Optimistic archive flip (mirrors `event_decide_local`): record the id so
      // the event leaves the resting queue at once, while `event_action{archive}`
      // is sent to the host; the committed archived event_upsert reconciles.
      const eventArchiveOverrides = state.eventArchiveOverrides.includes(action.eventId)
        ? state.eventArchiveOverrides
        : [...state.eventArchiveOverrides, action.eventId].slice(-200);
      return { ...state, eventArchiveOverrides };
    }

    case "event_unarchive_local": {
      // Optimistic unarchive: drop any pending archive override AND flip a
      // wire-archived event's local flag (the `event_read_local` pattern), so
      // the row returns to the resting queue at once whichever layer archived
      // it. The host's event_upsert echo reconciles the truth.
      const events = state.events.map((e) =>
        e.id === action.eventId && e.archived === true ? { ...e, archived: false } : e,
      );
      return {
        ...state,
        events,
        eventArchiveOverrides: state.eventArchiveOverrides.filter((id) => id !== action.eventId),
      };
    }

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
      // Keep the bare {clientId, body, ref} shape unless there is something extra
      // to carry, so the un-adorned case matches the original wire/replay path.
      const pendingSend: PendingSend = { clientId, body, ref };
      if (blobs.length > 0) pendingSend.attachments = blobs.map((b) => b.id);
      if (sendMode !== "send") pendingSend.mode = sendMode;
      if (mentionIds.length > 0) pendingSend.mentions = mentionIds;
      return {
        ...state,
        messages: capMessages([...state.messages, localMessage]),
        pendingSends: [...state.pendingSends, pendingSend],
        // v2.1 (ADR-0009): an anchored reply optimistically resolves its Ping.
        pings: resolveOpenPingByAnchor(state.pings, ref),
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

    // ---- v2.0 side chats (ADR-0008) ----
    // Every case below only ever touches `sideChats[sc]` / `sideChatRefs` /
    // `pendingSideSends` / `awaitingConclusions` — never `messages`,
    // `agentActivity`, or `turnEvents` — which is the structural guarantee
    // that sc-scoped routing can never leak into (or read from) main state.

    case "side_chat_open": {
      // Idempotent per Ping (protocol v2.0): resuming a live side chat answers
      // with the SAME sc and its transcript so far, so this both creates a
      // fresh scope and refreshes an already-hydrated one.
      const existing = state.sideChats[action.sc];
      const sideChat = existing
        ? { ...cloneSideChat(existing), messages: action.messages, ended: false }
        : freshSideChat(action.sc, action.pingId, action.messages);
      const sideChatRefs = state.sideChatRefs.some((r) => r.sc === action.sc)
        ? state.sideChatRefs
        : [...state.sideChatRefs, { sc: action.sc, ping_id: action.pingId }];
      return {
        ...state,
        sideChats: setSideChat(state.sideChats, action.sc, sideChat),
        sideChatRefs,
      };
    }

    case "side_chat_msg": {
      const sideChat = state.sideChats[action.sc];
      if (!sideChat) return state; // unknown/already-closed scope; drop.
      const cloned = cloneSideChat(sideChat);
      const { messages, reconciledClientId } = reconcileSideMessages(
        cloned.messages,
        action.message,
      );
      const commits = action.message.author === "agent";
      return {
        ...state,
        sideChats: setSideChat(state.sideChats, action.sc, {
          ...cloned,
          messages,
          turnEvents: commits ? [] : cloned.turnEvents,
        }),
        pendingSideSends: reconciledClientId
          ? state.pendingSideSends.filter((p) => p.clientId !== reconciledClientId)
          : state.pendingSideSends,
      };
    }

    case "side_chat_send_local": {
      const sideChat = state.sideChats[action.sc];
      if (!sideChat) return state;
      const localMessage: DisplayMessage = {
        id: action.localId,
        author: "owner",
        body: action.body,
        ref: action.ref,
        ts: action.ts,
        pending: true,
        clientId: action.clientId,
      };
      const cloned = cloneSideChat(sideChat);
      return {
        ...state,
        sideChats: setSideChat(state.sideChats, action.sc, {
          ...cloned,
          messages: [...cloned.messages, localMessage],
        }),
        pendingSideSends: [
          ...state.pendingSideSends,
          { sc: action.sc, clientId: action.clientId, body: action.body, ref: action.ref },
        ],
      };
    }

    case "side_chat_agent_activity": {
      const sideChat = state.sideChats[action.sc];
      if (!sideChat) return state;
      const cloned = cloneSideChat(sideChat);
      return {
        ...state,
        sideChats: setSideChat(state.sideChats, action.sc, {
          ...cloned,
          agentActivity: { state: action.state, text: action.text },
          turnEvents: action.state === "idle" ? [] : cloned.turnEvents,
        }),
      };
    }

    case "side_chat_turn_event": {
      const sideChat = state.sideChats[action.sc];
      if (!sideChat) return state;
      const cloned = cloneSideChat(sideChat);
      return {
        ...state,
        sideChats: setSideChat(state.sideChats, action.sc, {
          ...cloned,
          turnEvents: upsertTurnEvent(cloned.turnEvents, {
            seq: action.seq,
            event: action.event,
            at: Date.now(),
          }),
        }),
      };
    }

    case "side_chat_conclude_requested": {
      const sideChat = state.sideChats[action.sc];
      if (!sideChat) return state;
      return {
        ...state,
        sideChats: setSideChat(state.sideChats, action.sc, {
          ...cloneSideChat(sideChat),
          drafting: true,
        }),
      };
    }

    case "side_chat_conclusion_draft": {
      const sideChat = state.sideChats[action.sc];
      if (!sideChat) return state;
      return {
        ...state,
        sideChats: setSideChat(state.sideChats, action.sc, {
          ...cloneSideChat(sideChat),
          drafting: false,
          draft: action.text,
        }),
      };
    }

    case "side_chat_keep_editing": {
      // "Keep editing" returns to the side chat, never a discard: just clear
      // the draft so the confirmation sheet closes and the composer returns.
      const sideChat = state.sideChats[action.sc];
      if (!sideChat) return state;
      return {
        ...state,
        sideChats: setSideChat(state.sideChats, action.sc, {
          ...cloneSideChat(sideChat),
          draft: null,
        }),
      };
    }

    case "side_chat_confirm_sent": {
      const sideChat = state.sideChats[action.sc];
      if (!sideChat) return state;
      return {
        ...state,
        sideChats: setSideChat(state.sideChats, action.sc, {
          ...cloneSideChat(sideChat),
          confirming: true,
        }),
        awaitingConclusions: { ...state.awaitingConclusions, [action.anchor]: action.sc },
        // v2.1 (ADR-0009): a Side Chat Conclusion is an anchored reply too, so
        // it resolves its Ping optimistically on confirm (the confirm has no
        // send_local of its own — the owner reply lands later as a `msg`).
        pings: resolveOpenPingByAnchor(state.pings, action.anchor),
      };
    }

    case "side_chat_discard_sent": {
      const sideChat = state.sideChats[action.sc];
      if (!sideChat) return state;
      return {
        ...state,
        sideChats: setSideChat(state.sideChats, action.sc, {
          ...cloneSideChat(sideChat),
          discarding: true,
        }),
      };
    }

    case "side_chat_closed": {
      const sideChat = state.sideChats[action.sc];
      if (!sideChat) return state; // already gone (e.g. duplicate delivery).
      const sideChatRefs = state.sideChatRefs.filter((r) => r.sc !== action.sc);
      if (sideChat.confirming || sideChat.discarding) {
        // Expected close (conclude/discard) completed — drop the record. Every
        // remaining entry is cloned through rather than spread-aliased (see
        // cloneSideChat) since this rebuilds a fresh top-level map.
        const rest: AppState["sideChats"] = {};
        for (const [sc, other] of Object.entries(state.sideChats)) {
          if (sc !== action.sc) rest[sc] = cloneSideChat(other);
        }
        return { ...state, sideChats: rest, sideChatRefs };
      }
      // Host-initiated close we didn't ask for (TTL reap, or closed
      // elsewhere): mark terminal so a currently-open sheet shows "This side
      // chat ended" gracefully instead of the surface just vanishing.
      return {
        ...state,
        sideChats: setSideChat(state.sideChats, action.sc, {
          ...cloneSideChat(sideChat),
          ended: true,
        }),
        sideChatRefs,
      };
    }

    case "clear_last_conclusion":
      return { ...state, lastConclusion: null };

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
