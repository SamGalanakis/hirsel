// Single WebSocket client module: connect, hello/hello_ok, reconnect with
// exponential backoff, offline outgoing queue flushed on reconnect using
// stable client_ids so the host can dedupe resends. Also owns the v1.1 blob
// upload correlation and the v1.2 send-mode / cancel frames.
import type { Blob, ClientMessage, SendMode, ServerMessage } from "../protocol";
import { dispatch, state } from "../store/store";
import { jitteredDelayMs } from "./backoff";

const TOKEN_KEY = "hirsel.token";
const LAST_SEEN_KEY = "hirsel.lastSeenMsgId";

/** A pending send with no host echo after this long is surfaced as "failed"
 * with a retry affordance (spec: socket stays closed > 30s or send errors). */
const FAILED_AFTER_MS = 30_000;

/** Give up on an upload_blob whose blob_ok / error never arrives. */
const UPLOAD_TIMEOUT_MS = 45_000;

/** Give up on a get_blob_url whose blob_url / error never arrives (blocks an
 * image thumbnail / download link from resolving; fail into a placeholder). */
const BLOB_URL_TIMEOUT_MS = 20_000;

/** WebSocket close codes the host may use to reject a bad/expired token. The
 * canonical code isn't pinned in PROTOCOL.md yet (coordinate with the backend
 * lane — see report-web.md), so we match the plausible set: 1008 (policy
 * violation, the standard "your frame/credentials are unacceptable") plus the
 * 44xx app range hosts often use for auth. Any of these = auth failure with no
 * reconnect. */
const AUTH_REJECT_CODES = new Set([1008, 4001, 4401, 4403]);

/** Heuristic fallback for hosts that just drop the socket on a bad token with a
 * generic 1006/1000: if the VERY FIRST connection(s) close before we ever see a
 * `hello_ok`, that's a rejected token, not a flaky network (a token that ever
 * authenticated sets `everAuthed`, which permanently disables this path so a
 * real mid-session drop keeps reconnecting forever). Two strikes before we give
 * up, to ride out a one-off cold-start race. */
const MAX_CONNECTS_WITHOUT_HELLO = 2;

/** Signalled to the app when the token is rejected: the client has already
 * cleared the stored token and stopped reconnecting; the app clears its token
 * signal and routes back to the gate with `detail` as the inline error. */
export interface ClientHandlers {
  onAuthReject?: (detail: string) => void;
}

export function getStoredToken(): string | null {
  return localStorage.getItem(TOKEN_KEY);
}

export function setStoredToken(token: string): void {
  localStorage.setItem(TOKEN_KEY, token);
}

/** Drop this browser's stored credentials (token + replay cursor). Used by
 * Settings → "Forget token": the honest web analog of the Android client's
 * forget-device — the browser holds only the token, so clearing it returns the
 * app to the first-run gate. The caller reloads to tear the socket down. */
export function clearStoredToken(): void {
  localStorage.removeItem(TOKEN_KEY);
  localStorage.removeItem(LAST_SEEN_KEY);
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

/** Origin for out-of-band blob asset fetches, derived from the WS URL: ws→http,
 * wss→https, and a trailing `/ws` path dropped (the host serves the app + blobs
 * from the same origin root). The signed blob URL (D9) is host-relative, so this
 * prefixes it. */
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
  /** Unresolved get_blob_url promises, keyed by their client_id (D9). */
  private blobUrlReqs = new Map<string, { resolve: (url: string) => void; reject: (e: Error) => void }>();
  /** Per-pending-send "not echoed yet" timers, keyed by client_id. */
  private failTimers = new Map<string, ReturnType<typeof setTimeout>>();
  /** True once any `hello_ok` has arrived on this client — permanently disables
   * the "closed before hello ⇒ bad token" heuristic (see MAX_CONNECTS...). */
  private everAuthed = false;
  /** Consecutive connections that closed before a `hello_ok` (auth heuristic). */
  private connectsWithoutHello = 0;
  private handlers: ClientHandlers;

  constructor(url: string, token: string, handlers: ClientHandlers = {}) {
    this.url = url;
    this.token = token;
    this.handlers = handlers;
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

  /** Request a short-lived signed URL for a blob's bytes (D9), resolving with an
   * absolute, ready-to-fetch URL. Replaces the old `?token=` construction the
   * host now rejects — so `<img src>` / download links must resolve through
   * this. Fresh on every call (the URL expires ≈5 min out), so callers request
   * again at point of use rather than caching a URL that may have gone stale. */
  getBlobUrl(blobId: string): Promise<string> {
    const clientId = makeClientId();
    return new Promise<string>((resolve, reject) => {
      const timer = setTimeout(() => {
        if (this.blobUrlReqs.delete(clientId)) reject(new Error("blob url timed out"));
      }, BLOB_URL_TIMEOUT_MS);
      this.blobUrlReqs.set(clientId, {
        resolve: (url) => {
          clearTimeout(timer);
          resolve(url);
        },
        reject: (e) => {
          clearTimeout(timer);
          reject(e);
        },
      });
      // Enqueued so a request fired right as the socket blips still resolves once
      // reconnected (until the timeout), like upload_blob.
      this.enqueue({ type: "get_blob_url", client_id: clientId, blob_id: blobId });
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

    socket.addEventListener("close", (event) => {
      this.socket = null;
      if (this.closedByClient) return;
      // The precise, instant auth-reject signal is the pre-auth `error` frame
      // (handled in handleServerMessage); this close-side check is the FALLBACK
      // for a host that just drops the socket with no error frame — an explicit
      // reject code, or (only until the token has ever authenticated) the socket
      // closing before `hello_ok` too many times.
      const looksLikeAuthReject =
        AUTH_REJECT_CODES.has((event as CloseEvent).code) ||
        (!this.everAuthed && ++this.connectsWithoutHello >= MAX_CONNECTS_WITHOUT_HELLO);
      if (looksLikeAuthReject) {
        this.handleAuthReject();
        return;
      }
      dispatch({ type: "connection_status", status: "reconnecting" });
      this.scheduleReconnect();
    });

    socket.addEventListener("error", () => {
      socket.close();
    });
  }

  /** Terminal auth failure: stop everything, drop the stored token, and tell the
   * app to return to the gate with an error. Reconnecting would just re-reject
   * the same bad token forever (the C5 dead-end this replaces). `detail` is the
   * host's reason when we have one (a pre-auth `error` frame), else a generic
   * message for the socket-just-dropped heuristic path. */
  private handleAuthReject(detail?: string): void {
    if (this.closedByClient) return; // already torn down (e.g. error then close)
    this.closedByClient = true; // suppress any in-flight reconnect/close paths
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    for (const t of this.failTimers.values()) clearTimeout(t);
    this.failTimers.clear();
    this.socket?.close();
    clearStoredToken();
    dispatch({ type: "connection_status", status: "reconnecting" });
    this.handlers.onAuthReject?.(
      detail && detail.trim().length > 0
        ? detail
        : "Couldn't authenticate — check your token and try again.",
    );
  }

  private scheduleReconnect(): void {
    if (this.reconnectTimer) return;
    const delay = jitteredDelayMs(this.reconnectAttempt);
    this.reconnectAttempt += 1;
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.openSocket();
    }, delay);
  }

  private handleServerMessage(message: ServerMessage): void {
    switch (message.type) {
      case "hello_ok": {
        // The token authenticated: retire the bad-token heuristic for the rest
        // of this client's life so a later network drop reconnects, never gates.
        this.everAuthed = true;
        this.connectsWithoutHello = 0;
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
      case "blob_url": {
        const pending = this.blobUrlReqs.get(message.client_id);
        if (pending) {
          // Signed URL is host-relative; prefix the blob origin.
          pending.resolve(`${blobBase}${message.url}`);
          this.blobUrlReqs.delete(message.client_id);
        }
        break;
      }
      case "error": {
        // C5: the host rejects a bad hello with a plain `error` frame carrying a
        // reason and NO client_id, then closes the socket (no numeric close
        // code). Before this client has ever authenticated, that is an auth
        // rejection — act on it immediately (precise + instant) rather than
        // waiting out the close-before-hello heuristic. A correlated error
        // (upload/blob) always has a client_id and is handled below; a global
        // error that arrives AFTER authentication is a normal runtime error.
        if (!this.everAuthed && !message.client_id) {
          this.handleAuthReject(message.detail);
          break;
        }
        // An error carrying a client_id correlates to an in-flight upload or
        // blob-url request; reject its promise and mark the chip. Others are
        // surfaced to the log.
        if (message.client_id) {
          const pending = this.uploads.get(message.client_id);
          if (pending) {
            pending.reject(new Error(message.detail));
            this.uploads.delete(message.client_id);
          }
          const blobReq = this.blobUrlReqs.get(message.client_id);
          if (blobReq) {
            blobReq.reject(new Error(message.detail));
            this.blobUrlReqs.delete(message.client_id);
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

export function startClient(
  url: string,
  token: string,
  handlers: ClientHandlers = {},
): HirselWsClient {
  client?.close();
  blobBase = deriveBlobBase(url);
  client = new HirselWsClient(url, token, handlers);
  client.connect();
  return client;
}

export function getClient(): HirselWsClient | null {
  return client;
}

export type { HirselWsClient };
