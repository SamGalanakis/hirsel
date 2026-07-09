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
    opts?: { mode?: SendMode; attachments?: Blob[]; mentions?: number[] },
  ): number {
    const clientId = makeClientId();
    const localId = makeLocalId();
    const mode: SendMode = opts?.mode ?? "send";
    const attachments = opts?.attachments ?? [];
    const mentions = opts?.mentions ?? [];
    dispatch({
      type: "send_local",
      localId,
      clientId,
      body,
      ref,
      ts: new Date().toISOString(),
      attachments,
      mode,
      mentions,
    });
    this.sendFrame({
      type: "send_message",
      client_id: clientId,
      body,
      ref,
      attachments: attachments.map((b) => b.id),
      mode,
      // v2.1 (ADR-0009): @-mentioned ping ids. Omit when empty so the common
      // case keeps the pre-v2.1 wire shape (host defaults it to []).
      ...(mentions.length > 0 ? { mentions } : {}),
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
      ...(pending.mentions && pending.mentions.length > 0 ? { mentions: pending.mentions } : {}),
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

  /** v2.1: resolve a Ping to Done (⋯ "Mark done"). Enqueued so it survives an
   * offline window. (Was `archiveItem`.) */
  resolvePing(pingId: number): void {
    this.enqueue({ type: "resolve_ping", ping_id: pingId });
  }

  /** Mark a Ping read (v1.3). Optimistically flips read=true locally
   * (reconciled by the host's ping_upsert) and sends the idempotent read_ping
   * frame. Enqueued so it survives an offline window like resolve_ping. */
  readPing(pingId: number): void {
    dispatch({ type: "read_local", pingId });
    this.enqueue({ type: "read_ping", ping_id: pingId });
  }

  // ---- v2.0 side chats (ADR-0008) ----

  /** "Discuss" (fresh) or "Resume" (in-progress) — idempotent per Ping on the
   * host, so this is the single entry point for both. Enqueued so a tap right
   * as the socket drops still fires once reconnected. */
  openSideChat(pingId: number): void {
    this.enqueue({ type: "open_side_chat", client_id: makeClientId(), ping_id: pingId });
  }

  /** Send within a side chat's scope. Mirrors sendMessage's optimistic +
   * fail-timer-free durability model, but via the flat `pendingSideSends`
   * queue (see store/types.ts) instead of per-message fail timers — side
   * sends are text-only and this v1 keeps their offline story to "queued,
   * resent on reconnect" rather than replicating the full failed/retry chip. */
  sendSideMessage(sc: string, body: string, ref: number | null): number {
    const clientId = makeClientId();
    const localId = makeLocalId();
    dispatch({
      type: "side_chat_send_local",
      sc,
      localId,
      clientId,
      body,
      ref,
      ts: new Date().toISOString(),
    });
    this.sendFrame({ type: "send_message", client_id: clientId, body, ref, sc });
    return localId;
  }

  /** Cooperatively interrupt a side chat's active turn (Esc in the side
   * composer). Best-effort like the main cancelTurn — no-op if idle. */
  cancelSideTurn(sc: string): void {
    this.sendFrame({ type: "cancel_turn", sc });
  }

  /** "Conclude": have the side agent draft the Owner's reply. */
  concludeSideChat(sc: string): void {
    dispatch({ type: "side_chat_conclude_requested", sc });
    this.enqueue({ type: "conclude_side_chat", sc });
  }

  /** "Send reply" on the confirmation sheet: the Owner's edited-or-not final
   * text. `anchor` is the item's Anchor message id, recorded locally so the
   * plain owner reply this produces in main chat can be recognized (client-
   * derived provenance — the wire carries no marker) for the footer chip. */
  confirmConclusion(sc: string, text: string, anchor: number): void {
    dispatch({ type: "side_chat_confirm_sent", sc, anchor });
    this.enqueue({ type: "confirm_conclusion", sc, text });
  }

  /** Discard: end the side chat with no conclusion; the item stays open. */
  discardSideChat(sc: string): void {
    dispatch({ type: "side_chat_discard_sent", sc });
    this.enqueue({ type: "discard_side_chat", sc });
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
        // v2.0: sc-scoped frames route to their side chat and never touch
        // main state (structural guarantee — see reducer.ts). Absent `sc` is
        // byte-identical to pre-v2.0 main-chat handling.
        if (message.sc) {
          dispatch({ type: "side_chat_msg", sc: message.sc, message: message.message });
          break;
        }
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
        if (message.sc) {
          dispatch({
            type: "side_chat_agent_activity",
            sc: message.sc,
            state: message.state,
            text: message.text,
          });
          break;
        }
        dispatch({
          type: "agent_activity",
          payload: { state: message.state, text: message.text },
        });
        break;
      }
      case "ping_upsert": {
        dispatch({ type: "ping_upsert", payload: message });
        break;
      }
      case "process_upsert": {
        dispatch({ type: "process_upsert", payload: message });
        break;
      }
      case "turn_event": {
        if (message.sc) {
          dispatch({
            type: "side_chat_turn_event",
            sc: message.sc,
            seq: message.seq,
            event: message.event,
          });
          break;
        }
        dispatch({ type: "turn_event", payload: message });
        break;
      }
      case "side_chat_open": {
        dispatch({
          type: "side_chat_open",
          sc: message.sc,
          pingId: message.ping_id,
          messages: message.messages,
        });
        break;
      }
      case "conclusion_draft": {
        dispatch({ type: "side_chat_conclusion_draft", sc: message.sc, text: message.text });
        break;
      }
      case "side_chat_closed": {
        dispatch({ type: "side_chat_closed", sc: message.sc });
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
          ...(pending.mentions && pending.mentions.length > 0
            ? { mentions: pending.mentions }
            : {}),
        } satisfies ClientMessage),
      );
    }

    // v2.0: side-chat sends queued the same way (see openSideChat comment) —
    // "sends queued" while reconnecting applies to the side sheet too.
    for (const pending of state.pendingSideSends) {
      this.socket.send(
        JSON.stringify({
          type: "send_message",
          client_id: pending.clientId,
          body: pending.body,
          ref: pending.ref,
          sc: pending.sc,
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
