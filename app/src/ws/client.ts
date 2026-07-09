// Single WebSocket client module: connect, hello/hello_ok, reconnect with
// exponential backoff, offline outgoing queue flushed on reconnect using
// stable client_ids so the host can dedupe resends. Also owns the v1.1 blob
// upload correlation and the v1.2 send-mode / cancel frames.
import type { Blob, ClientMessage, SendMode, ServerMessage } from "../protocol";
import { dispatch, state } from "../store/store";
import { backoffDelayMs } from "./backoff";

const TOKEN_KEY = "hirsel.token";
const LAST_SEEN_KEY = "hirsel.lastSeenMsgId";

/** A pending send with no host echo after this long is surfaced as "failed"
 * with a retry affordance (spec: socket stays closed > 30s or send errors). */
const FAILED_AFTER_MS = 30_000;

/** Give up on an upload_blob whose blob_ok / error never arrives. */
const UPLOAD_TIMEOUT_MS = 45_000;

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

export function makeClientId(): string {
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

/** Base for out-of-band blob asset fetches (`GET /blob/{id}?token=…`), derived
 * from the WS URL: ws→http, wss→https, and a trailing `/ws` path dropped (the
 * host serves the app + blobs from the same origin root). */
let blobBase = "";
function deriveBlobBase(wsUrl: string): string {
  try {
    const u = new URL(wsUrl);
    u.protocol = u.protocol === "wss:" ? "https:" : "http:";
    u.pathname = u.pathname.replace(/\/ws\/?$/, "");
    return u.toString().replace(/\/$/, "");
  } catch {
    return "";
  }
}

/** URL for an attachment's content. Token is carried as a query param since the
 * fetch is a plain asset load (img src / anchor href), not a protocol frame. */
export function blobUrl(id: string): string {
  const token = getStoredToken() ?? "";
  return `${blobBase}/blob/${encodeURIComponent(id)}?token=${encodeURIComponent(token)}`;
}

class HirselWsClient {
  private url: string;
  private token: string;
  private socket: WebSocket | null = null;
  private reconnectAttempt = 0;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private closedByClient = false;
  private outbox: ClientMessage[] = [];
  /** Unresolved upload_blob promises, keyed by their client_id. */
  private uploads = new Map<string, { resolve: (b: Blob) => void; reject: (e: Error) => void }>();
  /** Per-pending-send "not echoed yet" timers, keyed by client_id. */
  private failTimers = new Map<string, ReturnType<typeof setTimeout>>();

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
    for (const t of this.failTimers.values()) clearTimeout(t);
    this.failTimers.clear();
    this.socket?.close();
  }

  /** Send a chat message. Returns the synthetic local id of the optimistic
   * entry so callers can request a scroll-to before the host echoes a real id. */
  sendMessage(
    body: string,
    ref: number | null,
    opts?: { mode?: SendMode; attachments?: Blob[] },
  ): number {
    const clientId = makeClientId();
    const localId = makeLocalId();
    const mode: SendMode = opts?.mode ?? "send";
    const attachments = opts?.attachments ?? [];
    dispatch({
      type: "send_local",
      localId,
      clientId,
      body,
      ref,
      ts: new Date().toISOString(),
      attachments,
      mode,
    });
    this.sendFrame({
      type: "send_message",
      client_id: clientId,
      body,
      ref,
      attachments: attachments.map((b) => b.id),
      mode,
    });
    this.armFailTimer(clientId);
    return localId;
  }

  /** Retry a still-pending send after it was surfaced as failed. */
  retrySend(clientId: string): void {
    const pending = state.pendingSends.find((p) => p.clientId === clientId);
    if (!pending) return;
    dispatch({ type: "send_retry", clientId });
    this.sendFrame({
      type: "send_message",
      client_id: pending.clientId,
      body: pending.body,
      ref: pending.ref,
      attachments: pending.attachments ?? [],
      mode: pending.mode ?? "send",
    });
    this.armFailTimer(clientId);
  }

  /** Upload a file's bytes; resolves with the stored Blob when blob_ok arrives,
   * rejects on an error frame carrying this client_id. The caller dispatches
   * upload_start first (so the chip renders) and reacts to the promise. */
  uploadBlob(clientId: string, name: string, mime: string, dataB64: string): Promise<Blob> {
    return new Promise<Blob>((resolve, reject) => {
      // Guard against a lost blob_ok or a host error frame that omits the
      // correlating client_id (the canonical error shape has no id): time out so
      // the chip fails into its retry state instead of the composer hanging.
      const timer = setTimeout(() => {
        if (this.uploads.delete(clientId)) reject(new Error("upload timed out"));
      }, UPLOAD_TIMEOUT_MS);
      this.uploads.set(clientId, {
        resolve: (b) => {
          clearTimeout(timer);
          resolve(b);
        },
        reject: (e) => {
          clearTimeout(timer);
          reject(e);
        },
      });
      this.enqueue({
        type: "upload_blob",
        client_id: clientId,
        name,
        mime,
        data_b64: dataB64,
      });
    });
  }

  cancelTurn(): void {
    this.sendFrame({ type: "cancel_turn" });
  }

  cancelQueued(clientId: string): void {
    this.sendFrame({ type: "cancel_queued", client_id: clientId });
  }

  archiveItem(itemId: number): void {
    this.enqueue({ type: "archive_item", item_id: itemId });
  }

  /** Mark an Inbox item read (v1.3). Optimistically flips read=true locally
   * (reconciled by the host's inbox_upsert) and sends the idempotent read_item
   * frame. Enqueued so it survives an offline window like archive_item. */
  readItem(itemId: number): void {
    dispatch({ type: "read_local", itemId });
    this.enqueue({ type: "read_item", item_id: itemId });
  }

  private armFailTimer(clientId: string): void {
    const existing = this.failTimers.get(clientId);
    if (existing) clearTimeout(existing);
    this.failTimers.set(
      clientId,
      setTimeout(() => {
        this.failTimers.delete(clientId);
        if (state.pendingSends.some((p) => p.clientId === clientId)) {
          dispatch({ type: "send_failed", clientId });
        }
      }, FAILED_AFTER_MS),
    );
  }

  /** Clear fail timers for sends that have since reconciled away. */
  private reconcileFailTimers(): void {
    for (const [clientId, timer] of this.failTimers) {
      if (!state.pendingSends.some((p) => p.clientId === clientId)) {
        clearTimeout(timer);
        this.failTimers.delete(clientId);
      }
    }
  }

  private enqueue(frame: ClientMessage): void {
    if (this.socket && this.socket.readyState === WebSocket.OPEN) {
      this.socket.send(JSON.stringify(frame));
    } else {
      this.outbox.push(frame);
    }
  }

  /** Best-effort immediate send; dropped if the socket is not open right now
   * (send_message durability comes from the pendingSends replay). */
  private sendFrame(frame: ClientMessage): void {
    if (this.socket && this.socket.readyState === WebSocket.OPEN) {
      this.socket.send(JSON.stringify(frame));
    }
  }

  private openSocket(): void {
    dispatch({
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
      dispatch({ type: "connection_status", status: "reconnecting" });
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
    switch (message.type) {
      case "hello_ok": {
        dispatch({ type: "hello_ok", payload: message });
        setStoredLastSeen(message.latest_msg_id);
        dispatch({ type: "connection_status", status: "connected" });
        this.reconnectAttempt = 0;
        this.reconcileFailTimers();
        this.flushOutbox();
        break;
      }
      case "msg": {
        dispatch({ type: "msg", payload: message });
        setStoredLastSeen(message.message.id);
        this.reconcileFailTimers();
        break;
      }
      case "msg_removed": {
        dispatch({ type: "msg_removed", id: message.id });
        this.reconcileFailTimers();
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
      case "process_upsert": {
        dispatch({ type: "process_upsert", payload: message });
        break;
      }
      case "turn_event": {
        dispatch({ type: "turn_event", payload: message });
        break;
      }
      case "blob_ok": {
        const pending = this.uploads.get(message.client_id);
        if (pending) {
          pending.resolve(message.blob);
          this.uploads.delete(message.client_id);
        }
        dispatch({ type: "blob_ok", clientId: message.client_id, blob: message.blob });
        break;
      }
      case "error": {
        // An error carrying a client_id correlates to an in-flight upload;
        // reject its promise and mark the chip. Others are surfaced to the log.
        if (message.client_id) {
          const pending = this.uploads.get(message.client_id);
          if (pending) {
            pending.reject(new Error(message.detail));
            this.uploads.delete(message.client_id);
          }
          dispatch({ type: "upload_error", clientId: message.client_id });
        }
        // eslint-disable-next-line no-console
        console.error("hirsel protocol error:", message.detail);
        break;
      }
    }
  }

  private flushOutbox(): void {
    if (!this.socket || this.socket.readyState !== WebSocket.OPEN) return;

    // Resend anything still un-acked, oldest first, using its original
    // client_id so the host can dedupe if it received it before the disconnect.
    for (const pending of state.pendingSends) {
      this.socket.send(
        JSON.stringify({
          type: "send_message",
          client_id: pending.clientId,
          body: pending.body,
          ref: pending.ref,
          attachments: pending.attachments ?? [],
          mode: pending.mode ?? "send",
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
  blobBase = deriveBlobBase(url);
  client = new HirselWsClient(url, token);
  client.connect();
  return client;
}

export function getClient(): HirselWsClient | null {
  return client;
}

export type { HirselWsClient };
