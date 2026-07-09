// Pure reducer over the client's protocol-facing state. Kept free of any
// WebSocket concerns so it can be unit tested directly (see reducer.test.ts)
// and reused verbatim by the Solid store.
import type { InboxItem } from "../protocol";
import type { Action, AppState, DisplayMessage, PendingSend, Upload } from "./types";

function upsertInboxItem(inbox: InboxItem[], item: InboxItem): InboxItem[] {
  const idx = inbox.findIndex((existing) => existing.id === item.id);
  if (idx === -1) return [...inbox, item];
  const next = inbox.slice();
  next[idx] = item;
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

      return {
        ...state,
        messages: [...merged, ...pending],
        inbox,
        lastSeenMsgId: latest_msg_id,
        pendingSends,
      };
    }

    case "msg": {
      const message = action.payload.message;
      // A tombstoned id (cancelled queued message) must never re-materialize,
      // even if its echo arrives after the msg_removed that killed it.
      if (state.removedIds.includes(message.id)) return state;
      const next = reconcileOrAppend(state, message);
      return {
        ...next,
        lastSeenMsgId:
          state.lastSeenMsgId === null ? message.id : Math.max(state.lastSeenMsgId, message.id),
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
      };

    case "inbox_upsert":
      return {
        ...state,
        inbox: upsertInboxItem(state.inbox, action.payload.item),
      };

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

    default:
      return state;
  }
}
