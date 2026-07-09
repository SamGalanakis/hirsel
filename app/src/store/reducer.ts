// Pure reducer over the client's protocol-facing state. Kept free of any
// WebSocket concerns so it can be unit tested directly (see reducer.test.ts)
// and reused verbatim by the Solid store.
import type { ChatMessage, InboxItem, ProcessInfo } from "../protocol";
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

function upsertInboxItem(inbox: InboxItem[], item: InboxItem): InboxItem[] {
  const idx = inbox.findIndex((existing) => existing.id === item.id);
  if (idx === -1) return [...inbox, item];
  const next = inbox.slice();
  next[idx] = item;
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

  return { ...state, messages: [...state.messages, message] };
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
function freshSideChat(sc: string, itemId: number, messages: ChatMessage[]): SideChatState {
  return {
    sc,
    itemId,
    messages,
    agentActivity: { state: "idle", text: null },
    turnEvents: [],
    drafting: false,
    draft: null,
    confirming: false,
    discarding: false,
    itemArchived: false,
    ended: false,
  };
}

export function reduce(state: AppState, action: Action): AppState {
  switch (action.type) {
    case "hello_ok": {
      const { latest_msg_id, inbox } = action.payload;
      // Never re-admit a tombstoned (cancelled) id from replay.
      const messages = action.payload.messages.filter((m) => !state.removedIds.includes(m.id));
      const known = new Map<number, DisplayMessage>();
      for (const m of state.messages) {
        if (!m.pending) known.set(m.id, m);
      }
      const newlyReplayed = messages.filter((m) => !known.has(m.id));
      for (const m of messages) known.set(m.id, m);
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
        messages: [...merged, ...pending],
        inbox,
        lastSeenMsgId: latest_msg_id,
        pendingSends,
        // Fresh sync boundary: seed processes; the live turn timeline (ephemeral,
        // never replayed) does not survive a resync. Retained turn details for
        // already-shown messages are kept (session memory, orthogonal to sync).
        processes: action.payload.processes ?? [],
        turnEvents: [],
        sideChatRefs,
        sideChats,
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
        }),
      };

    case "inbox_upsert": {
      const item = action.payload.item;
      // Edge case (critique, binding): the Agent can archive an item while its
      // side chat is still open. Don't kill the sheet — flag a non-blocking
      // banner; Conclude/Discard both remain available. Fully derivable here,
      // so no separate action/dispatch is needed for it.
      let sideChats = state.sideChats;
      if (item.status === "archived") {
        const hasArchivable = Object.values(state.sideChats).some(
          (sc) => sc.itemId === item.id && !sc.itemArchived,
        );
        // Guarded so the common case (no matching live side chat) leaves
        // `sideChats` as the untouched live reference — cheap, and safe,
        // since nothing rebuilds a fresh wrapper around it in that case. Once
        // something IS changing, every entry (not just the matching one) is
        // cloned through, not just aliased into the fresh map (see
        // cloneSideChat's note on why a live reference can't ride along).
        if (hasArchivable) {
          const next: AppState["sideChats"] = {};
          for (const [sc, sideChat] of Object.entries(state.sideChats)) {
            next[sc] =
              sideChat.itemId === item.id && !sideChat.itemArchived
                ? { ...cloneSideChat(sideChat), itemArchived: true }
                : cloneSideChat(sideChat);
          }
          sideChats = next;
        }
      }
      return {
        ...state,
        inbox: upsertInboxItem(state.inbox, item),
        sideChats,
      };
    }

    case "read_local": {
      // Optimistic email-like "seen" flip: set read=true locally (the host's
      // inbox_upsert reconciles it) and drop any manual unread override, since
      // reading always wins over a prior "Mark unread".
      const inbox = state.inbox.map((i) =>
        i.id === action.itemId ? { ...i, read: true } : i,
      );
      return {
        ...state,
        inbox,
        unreadOverrides: state.unreadOverrides.filter((id) => id !== action.itemId),
      };
    }

    case "mark_unread_local": {
      // Client-only override (no wire unread op): record the id so the item is
      // rendered/counted as unread even though the wire `read` flag stays true.
      const unreadOverrides = state.unreadOverrides.includes(action.itemId)
        ? state.unreadOverrides
        : [...state.unreadOverrides, action.itemId].slice(-200);
      return { ...state, unreadOverrides };
    }

    case "send_local": {
      const { localId, clientId, body, ref, ts, attachments, mode } = action;
      const blobs = attachments ?? [];
      const sendMode = mode ?? "send";
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
      return {
        ...state,
        messages: [...state.messages, localMessage],
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

    // ---- v2.0 side chats (ADR-0008) ----
    // Every case below only ever touches `sideChats[sc]` / `sideChatRefs` /
    // `pendingSideSends` / `awaitingConclusions` — never `messages`,
    // `agentActivity`, or `turnEvents` — which is the structural guarantee
    // that sc-scoped routing can never leak into (or read from) main state.

    case "side_chat_open": {
      // Idempotent per item (protocol v2.0): resuming a live side chat answers
      // with the SAME sc and its transcript so far, so this both creates a
      // fresh scope and refreshes an already-hydrated one.
      const existing = state.sideChats[action.sc];
      const sideChat = existing
        ? { ...cloneSideChat(existing), messages: action.messages, ended: false }
        : freshSideChat(action.sc, action.itemId, action.messages);
      const sideChatRefs = state.sideChatRefs.some((r) => r.sc === action.sc)
        ? state.sideChatRefs
        : [...state.sideChatRefs, { sc: action.sc, item_id: action.itemId }];
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
          turnEvents: upsertTurnEvent(cloned.turnEvents, { seq: action.seq, event: action.event }),
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

    default:
      return state;
  }
}
