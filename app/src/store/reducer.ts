// Pure reducer over the client's protocol-facing state. Kept free of any
// WebSocket/zustand concerns so it can be unit tested directly (see
// reducer.test.ts) and reused verbatim by the zustand store.
import type { InboxItem } from "../protocol";
import type { Action, AppState, DisplayMessage } from "./types";

function upsertInboxItem(inbox: InboxItem[], item: InboxItem): InboxItem[] {
  const idx = inbox.findIndex((existing) => existing.id === item.id);
  if (idx === -1) return [...inbox, item];
  const next = inbox.slice();
  next[idx] = item;
  return next;
}

/** Reconcile an incoming owner-authored `msg` against the oldest still-pending
 * optimistic send whose body matches (protocol.md: "replaces optimistic entry
 * with the first msg whose author=owner and body matches"). */
function reconcileOrAppend(state: AppState, message: DisplayMessage): AppState {
  if (state.messages.some((m) => !m.pending && m.id === message.id)) {
    // Already known (e.g. re-delivered); nothing to do.
    return state;
  }

  if (message.author === "owner") {
    const pendingIdx = state.messages.findIndex(
      (m) => m.pending && m.body === message.body,
    );
    if (pendingIdx !== -1) {
      const reconciled = state.messages[pendingIdx];
      const nextMessages = state.messages.slice();
      nextMessages[pendingIdx] = { ...message };
      return {
        ...state,
        messages: nextMessages,
        pendingSends: state.pendingSends.filter(
          (p) => p.clientId !== reconciled.clientId,
        ),
      };
    }
  }

  return { ...state, messages: [...state.messages, message] };
}

export function reduce(state: AppState, action: Action): AppState {
  switch (action.type) {
    case "hello_ok": {
      const { latest_msg_id, messages, inbox } = action.payload;
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
      // Same rule as reconcileOrAppend - each newly replayed owner message
      // consumes the oldest still-pending entry with a matching body. Without
      // this the message would render twice (replayed + stuck pending bubble)
      // and its client_id would be resent on every future reconnect forever.
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
      const next = reconcileOrAppend(state, message);
      return {
        ...next,
        lastSeenMsgId:
          state.lastSeenMsgId === null
            ? message.id
            : Math.max(state.lastSeenMsgId, message.id),
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

    case "send_local": {
      const { localId, clientId, body, ref, ts } = action;
      // Negative synthetic ids (assigned by the caller, see ws/client.ts) keep
      // optimistic entries clearly out of the host's id space; they are never
      // sorted against real ids, only ever appended at the tail and later
      // replaced in place once reconciled.
      const localMessage: DisplayMessage = {
        id: localId,
        author: "owner",
        body,
        ref,
        ts,
        pending: true,
        clientId,
      };
      return {
        ...state,
        messages: [...state.messages, localMessage],
        pendingSends: [...state.pendingSends, { clientId, body, ref }],
      };
    }

    case "connection_status":
      return { ...state, connection: action.status };

    default:
      return state;
  }
}
