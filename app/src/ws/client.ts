// Single WebSocket client module: connect, hello/hello_ok, reconnect with
// exponential backoff, offline outgoing queue flushed on reconnect using
// stable client_ids so the host can dedupe resends.
import type { ClientMessage, ServerMessage } from "../protocol";
import { storeApi } from "../store/store";
import { backoffDelayMs } from "./backoff";

const TOKEN_KEY = "hirsel.token";
const LAST_SEEN_KEY = "hirsel.lastSeenMsgId";

export function getStoredToken(): string | null {
  return localStorage.getItem(TOKEN_KEY);
}

export function setStoredToken(token: string): void {
  localStorage.setItem(TOKEN_KEY, token);
}

function getStoredLastSeen(): number | null {
  const raw = localStorage.getItem(LAST_SEEN_KEY);
  if (raw === null) return null;
  const parsed = Number(raw);
  return Number.isFinite(parsed) ? parsed : null;
}

function setStoredLastSeen(id: number): void {
  localStorage.setItem(LAST_SEEN_KEY, String(id));
}

function makeClientId(): string {
  return crypto.randomUUID();
}

// Negative, monotonically-decreasing synthetic ids for optimistic messages -
// always outside the host's (positive, monotonic) id space.
let nextLocalId = -1;
function makeLocalId(): number {
  const id = nextLocalId;
  nextLocalId -= 1;
  return id;
}

class HirselWsClient {
  private url: string;
  private token: string;
  private socket: WebSocket | null = null;
  private reconnectAttempt = 0;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private closedByClient = false;
  /** Raw non-send_message frames queued while the socket is not open (e.g.
   * archive_item), flushed in order once hello_ok confirms the session.
   * send_message frames are not queued here: `pendingSends` in the store is
   * their source of truth, and every un-acked entry there is (re)sent after
   * every hello_ok, whether this is the first connect or a reconnect - the
   * host dedupes by client_id, so resending an already-acked id is a no-op. */
  private outbox: ClientMessage[] = [];

  constructor(url: string, token: string) {
    this.url = url;
    this.token = token;
  }

  connect(): void {
    this.closedByClient = false;
    this.openSocket();
  }

  close(): void {
    this.closedByClient = true;
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    this.socket?.close();
  }

  /** Returns the synthetic local id assigned to the optimistic message, so
   * callers (e.g. Quick Reply, Reply-to-anchor) can request a scroll-to it
   * before the host has echoed back a real id. */
  sendMessage(body: string, ref: number | null): number {
    const clientId = makeClientId();
    const localId = makeLocalId();
    storeApi.getState().dispatch({
      type: "send_local",
      localId,
      clientId,
      body,
      ref,
      ts: new Date().toISOString(),
    });
    this.sendFrame({ type: "send_message", client_id: clientId, body, ref });
    return localId;
  }

  archiveItem(itemId: number): void {
    this.enqueue({ type: "archive_item", item_id: itemId });
  }

  private enqueue(frame: ClientMessage): void {
    if (this.socket && this.socket.readyState === WebSocket.OPEN) {
      this.socket.send(JSON.stringify(frame));
    } else {
      this.outbox.push(frame);
    }
  }

  /** Best-effort immediate send; silently dropped if the socket is not open
   * right now (the caller is responsible for durability - send_message relies
   * on pendingSends replay, see flushOutbox). */
  private sendFrame(frame: ClientMessage): void {
    if (this.socket && this.socket.readyState === WebSocket.OPEN) {
      this.socket.send(JSON.stringify(frame));
    }
  }

  private openSocket(): void {
    storeApi.getState().dispatch({
      type: "connection_status",
      status: this.reconnectAttempt === 0 ? "connecting" : "reconnecting",
    });

    const socket = new WebSocket(this.url);
    this.socket = socket;

    socket.addEventListener("open", () => {
      const hello: ClientMessage = {
        type: "hello",
        token: this.token,
        last_seen_msg_id: getStoredLastSeen(),
      };
      socket.send(JSON.stringify(hello));
    });

    socket.addEventListener("message", (event) => {
      this.handleServerMessage(JSON.parse(event.data as string) as ServerMessage);
    });

    socket.addEventListener("close", () => {
      this.socket = null;
      if (this.closedByClient) return;
      storeApi.getState().dispatch({ type: "connection_status", status: "reconnecting" });
      this.scheduleReconnect();
    });

    socket.addEventListener("error", () => {
      socket.close();
    });
  }

  private scheduleReconnect(): void {
    if (this.reconnectTimer) return;
    const delay = backoffDelayMs(this.reconnectAttempt);
    this.reconnectAttempt += 1;
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.openSocket();
    }, delay);
  }

  private handleServerMessage(message: ServerMessage): void {
    const dispatch = storeApi.getState().dispatch;

    switch (message.type) {
      case "hello_ok": {
        dispatch({ type: "hello_ok", payload: message });
        setStoredLastSeen(message.latest_msg_id);
        dispatch({ type: "connection_status", status: "connected" });
        this.reconnectAttempt = 0;
        this.flushOutbox();
        break;
      }
      case "msg": {
        dispatch({ type: "msg", payload: message });
        setStoredLastSeen(message.message.id);
        break;
      }
      case "agent_activity": {
        dispatch({
          type: "agent_activity",
          payload: { state: message.state, text: message.text },
        });
        break;
      }
      case "inbox_upsert": {
        dispatch({ type: "inbox_upsert", payload: message });
        break;
      }
      case "error": {
        // Slice 1: surface via activity text so it is at least visible.
        // eslint-disable-next-line no-console
        console.error("hirsel protocol error:", message.detail);
        break;
      }
    }
  }

  private flushOutbox(): void {
    if (!this.socket || this.socket.readyState !== WebSocket.OPEN) return;

    // Resend anything still un-acked, oldest first, using its original
    // client_id so the host can dedupe if it actually did receive it before
    // the disconnect.
    for (const pending of storeApi.getState().pendingSends) {
      this.socket.send(
        JSON.stringify({
          type: "send_message",
          client_id: pending.clientId,
          body: pending.body,
          ref: pending.ref,
        } satisfies ClientMessage),
      );
    }

    const queued = this.outbox;
    this.outbox = [];
    for (const frame of queued) {
      this.socket.send(JSON.stringify(frame));
    }
  }
}

let client: HirselWsClient | null = null;

export function startClient(url: string, token: string): HirselWsClient {
  client?.close();
  client = new HirselWsClient(url, token);
  client.connect();
  return client;
}

export function getClient(): HirselWsClient | null {
  return client;
}
