import type {
  AgentActivityState,
  ChatMessage,
  HelloOkMsg,
  InboxItem,
  InboxUpsertMsg,
  MsgMsg,
} from "../protocol";

/** A chat message as rendered locally. `pending`/`clientId` only exist on
 * optimistic entries created by this client before the host has acked them. */
export type DisplayMessage = ChatMessage & {
  pending?: boolean;
  clientId?: string;
};

/** An outgoing `send_message` this client has emitted but not yet seen
 * reconciled via a server `msg` echo. Kept so reconnect can resend with the
 * same `client_id` (host dedupes) and so we know what to reconcile against. */
export interface PendingSend {
  clientId: string;
  body: string;
  ref: number | null;
}

export type ConnectionStatus = "connecting" | "connected" | "reconnecting";

export interface AgentActivity {
  state: AgentActivityState;
  text: string | null;
}

export interface AppState {
  messages: DisplayMessage[];
  inbox: InboxItem[];
  agentActivity: AgentActivity;
  connection: ConnectionStatus;
  lastSeenMsgId: number | null;
  pendingSends: PendingSend[];
}

export type Action =
  | { type: "hello_ok"; payload: HelloOkMsg }
  | { type: "msg"; payload: MsgMsg }
  | { type: "agent_activity"; payload: { state: AgentActivityState; text: string | null } }
  | { type: "inbox_upsert"; payload: InboxUpsertMsg }
  | {
      type: "send_local";
      localId: number;
      clientId: string;
      body: string;
      ref: number | null;
      ts: string;
    }
  | { type: "connection_status"; status: ConnectionStatus };

export function initialState(): AppState {
  return {
    messages: [],
    inbox: [],
    agentActivity: { state: "idle", text: null },
    connection: "connecting",
    lastSeenMsgId: null,
    pendingSends: [],
  };
}
