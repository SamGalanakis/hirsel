// Single WebSocket client module: connect, hello/hello_ok, reconnect with
// exponential backoff, offline outgoing queue flushed on reconnect using
// stable client_ids so the host can dedupe resends. Also owns the v1.1 blob
// upload correlation and the v1.2 send-mode / cancel frames.
import type {
  AgentSlot,
  Blob,
  ClientMessage,
  MessagesMsg,
  SendMode,
  ServerMessage,
} from "../protocol";
import { httpBaseFromWs } from "../lib/endpoint";
import { deliverPluginPush } from "../plugins/registry";
import { dispatch, setProtocolError, state } from "../store/store";
import type { PendingSend } from "../store/types";
import { jitteredDelayMs } from "./backoff";

/** THE `send_message` frame builder — the one place the wire shape is spelled.
 * Three call sites used to spell it out: the first send, the manual retry, and
 * the reconnect outbox flush, each re-applying the same defaults and the same
 * `mentions` omission. `PendingSend` is total, so all this does is apply the one
 * wire omission there is: an empty `mentions` is dropped, keeping the pre-v2.1
 * shape a host without the field expects. */
function sendMessageFrame(pending: PendingSend): ClientMessage {
  return {
    type: "send_message",
    client_id: pending.clientId,
    body: pending.body,
    ref: pending.ref,
    attachments: pending.attachments,
    mode: pending.mode,
    ...(pending.mentions.length > 0 ? { mentions: pending.mentions } : {}),
  };
}

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

/** Give up on a history page whose correlated `messages` response never arrives. */
const HISTORY_TIMEOUT_MS = 20_000;

/** WebSocket close codes the host may use to reject a bad/expired token. The
 * canonical code isn't pinned in PROTOCOL.md yet (coordinate with the backend
 * lane — see report-web.md), so we match the plausible set: 1008 (policy
 * violation, the standard "your frame/credentials are unacceptable") plus the
 * 44xx app range hosts often use for auth. Any of these = auth failure with no
 * reconnect. */
const AUTH_REJECT_CODES = new Set([1008, 4001, 4401, 4403]);

/** Heuristic fallback for hosts that accept a socket, then drop it on a bad
 * token with a generic 1006/1000 before `hello_ok`. Only sockets that reached
 * OPEN count: a connection refused before OPEN means the host is absent, so it
 * keeps reconnecting without striking. A token that ever authenticated sets
 * `everAuthed`, permanently disabling this path so real mid-session drops keep
 * reconnecting forever. Two accepted-then-dropped strikes before we give up. */
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
  /** Exactly one history request may be in flight; repeated top-edge scroll
   * events share its promise instead of emitting duplicate pages. */
  private historyRequest: {
    clientId: string;
    promise: Promise<MessagesMsg>;
    resolve: (page: MessagesMsg) => void;
    reject: (error: Error) => void;
    timer: ReturnType<typeof setTimeout>;
  } | null = null;
  /** Per-pending-send "not echoed yet" timers, keyed by client_id. */
  private failTimers = new Map<string, ReturnType<typeof setTimeout>>();
  /** True once any `hello_ok` has arrived on this client — permanently disables
   * the "closed before hello ⇒ bad token" heuristic (see MAX_CONNECTS...). */
  private everAuthed = false;
  /** Accepted connections that closed before a `hello_ok` (auth heuristic). */
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

  /** Send a conversation message. Returns the synthetic local id of the optimistic
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
    this.sendFrame(
      sendMessageFrame({
        clientId,
        body,
        ref,
        attachments: attachments.map((b) => b.id),
        mode,
        mentions,
      }),
    );
    this.armFailTimer(clientId);
    return localId;
  }

  /** Retry a still-pending send after it was surfaced as failed. */
  retrySend(clientId: string): void {
    const pending = state.pendingSends.find((p) => p.clientId === clientId);
    if (!pending) return;
    dispatch({ type: "send_retry", clientId });
    this.sendFrame(sendMessageFrame(pending));
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

  /** Fetch one page immediately before `beforeId`. Calls made while a page is
   * pending share that correlated request, enforcing the single-flight guard
   * below scroll-event frequency as well as in the component decision helper. */
  fetchMessages(beforeId: number, limit: number): Promise<MessagesMsg> {
    if (this.historyRequest) return this.historyRequest.promise;
    const clientId = makeClientId();
    let resolvePage!: (page: MessagesMsg) => void;
    let rejectPage!: (error: Error) => void;
    const promise = new Promise<MessagesMsg>((resolve, reject) => {
      resolvePage = resolve;
      rejectPage = reject;
    });
    const timer = setTimeout(() => {
      if (this.historyRequest?.clientId !== clientId) return;
      this.historyRequest = null;
      rejectPage(new Error("history request timed out"));
    }, HISTORY_TIMEOUT_MS);
    this.historyRequest = {
      clientId,
      promise,
      resolve: resolvePage,
      reject: rejectPage,
      timer,
    };
    this.enqueue({
      type: "fetch_messages",
      client_id: clientId,
      before_id: beforeId,
      limit,
    });
    return promise;
  }

  cancelTurn(): void {
    this.sendFrame({ type: "cancel_turn" });
  }

  cancelQueued(clientId: string): void {
    this.sendFrame({ type: "cancel_queued", client_id: clientId });
  }

  /** Mark a Task read through the legacy wire id space and operation,
   * while the event reducer owns the local optimistic flip. */
  readEvent(eventId: number): void {
    this.enqueue({ type: "read_ping", ping_id: eventId });
  }

  // ---- Generative-UI tier (view templates) ----

  /** Emit an owner-initiated event from an interactive view component
   * (`action` / `optionSet` / `form`). Enqueued so a tap right as the socket
   * blips still fires once reconnected (like resolve_ping). The reply returns
   * through the ordinary conversation/Task flow — there is no direct ack — so callers show
   * a brief local pending state and let the resulting msg/ping_upsert land
   * normally. The client never creates messages or settles Tasks itself. */
  sendViewEvent(instanceId: string, action: string, data: unknown): void {
    this.enqueue({ type: "view_event", instance_id: instanceId, action, data });
  }

  // ---- Task actions (typed Event compatibility wire) ----

  /** Emit an owner action from an event card (`choose` / `submit` / `snooze` /
   * `dismiss` / `reopen`). Generalizes the quick-reply resolution + view_event.
   * Enqueued so a tap right as the socket blips still fires once reconnected;
   * there is no direct ack — the reply returns through the normal event flow (a
   * `done` event_upsert), so callers show a brief optimistic state and let it
   * land. The client never resolves the event itself. */
  sendEventAction(eventId: number, action: string, data: unknown): void {
    this.enqueue({ type: "event_action", event_id: eventId, action, data });
  }

  /** Sweep every finished event out of the resting queue in one op (Wave-3 "Clear
   * finished"). Enqueued like `sendEventAction` so a tap right as the socket
   * blips still lands once reconnected; the host archives the finished set and
   * echoes an `archived` `event_upsert` per event, reconciling the client's
   * optimistic batch archive. No direct ack. */
  clearFinishedEvents(): void {
    this.enqueue({ type: "clear_finished_events" });
  }

  // ---- Model configuration ----

  /** Select the main agent's model + reasoning variant. Enqueued so a tap right
   * as the socket blips still lands once reconnected; the UI shows a brief
   * pending state and settles on the `model_changed` broadcast (no permanent
   * optimistic divergence — the store is only written by the broadcast). */
  setModel(modelId: string, variant: string): void {
    this.enqueue({ type: "set_model", model_id: modelId, variant });
  }

  /** Update one sub-agent catalog model's full row state (master enabled flag +
   * enabled reasoning variants). Settles on `subagent_models_changed`. */
  setSubagentModel(
    provider: string,
    modelId: string,
    enabled: boolean,
    enabledVariants: string[],
  ): void {
    this.enqueue({
      type: "set_subagent_model",
      provider,
      model_id: modelId,
      enabled,
      enabled_variants: enabledVariants,
    });
  }

  /** Persist the main Agent's editable prompt body. Empty resets to bundled. */
  setAgentPrompt(text: string): void {
    this.enqueue({ type: "set_agent_prompt", text });
  }

  /** Persist the incoming-event fork's editable prompt body. */
  setForkPrompt(text: string): void {
    this.enqueue({ type: "set_fork_prompt", text });
  }

  /** Select the incoming-event fork's model + reasoning variant. */
  setForkModel(modelId: string, variant: string): void {
    this.enqueue({ type: "set_fork_model", model_id: modelId, variant });
  }

  // ---- Provider roster ----

  /** Point one resident agent at a provider instance. The host seeds that
   * provider's default model and broadcasts the resulting snapshots. */
  setAgentProvider(agent: AgentSlot, providerId: string): void {
    this.enqueue({ type: "set_agent_provider", agent, provider_id: providerId });
  }

  /** Add an OpenAI-compatible provider instance. Settles on
   * `providers_changed`, like every other roster write. */
  addProvider(instance: {
    id: string;
    label: string;
    base_url: string;
    api_key: string;
    default_model: string;
  }): void {
    this.enqueue({ type: "add_provider", ...instance });
  }

  /** Edit one instance. Omitted fields are unchanged; an `api_key` of `""`
   * clears the stored key, and omitting it leaves the stored key alone — the
   * client never holds key material to resend. */
  updateProvider(
    id: string,
    patch: { label?: string; base_url?: string; api_key?: string; default_model?: string },
  ): void {
    this.enqueue({ type: "update_provider", id, ...patch });
  }

  /** Remove a removable instance. */
  removeProvider(id: string): void {
    this.enqueue({ type: "remove_provider", id });
  }

  /** Re-probe an OAuth provider's local credentials on the host machine. */
  redetectProvider(id: string): void {
    this.enqueue({ type: "redetect_provider", id });
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
    let opened = false;

    socket.addEventListener("open", () => {
      opened = true;
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
      // reject code, or (only until the token has ever authenticated) an opened
      // socket closing before `hello_ok` too many times. Never-opened sockets
      // mean network absence and reconnect without consuming a strike.
      const looksLikeAuthReject =
        AUTH_REJECT_CODES.has((event as CloseEvent).code) ||
        (opened &&
          !this.everAuthed &&
          ++this.connectsWithoutHello >= MAX_CONNECTS_WITHOUT_HELLO);
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
        dispatch({ type: "msg", payload: message });
        setStoredLastSeen(message.message.id);
        this.reconcileFailTimers();
        break;
      }
      case "messages": {
        const pending = this.historyRequest;
        if (pending?.clientId !== message.client_id) break;
        clearTimeout(pending.timer);
        this.historyRequest = null;
        pending.resolve(message);
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
      case "ping_upsert":
        break; // Legacy wire frame; typed events are authoritative.
      case "event_upsert": {
        dispatch({ type: "event_upsert", payload: message });
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
      case "view_upsert": {
        dispatch({ type: "view_upsert", payload: message });
        break;
      }
      case "view_removed": {
        dispatch({ type: "view_removed", payload: message });
        break;
      }
      case "model_changed": {
        dispatch({ type: "model_changed", model: message.model });
        break;
      }
      case "plugin_push": {
        // Plugin data is not app state: it never enters the reducer/store.
        // The registry fans the frame out to that plugin's `onPush` handlers
        // and drops it when nobody is listening.
        deliverPluginPush(message);
        break;
      }
      case "subagent_models_changed": {
        dispatch({ type: "subagent_models_changed", catalog: message.catalog });
        break;
      }
      case "prompts_changed": {
        dispatch({ type: "prompts_changed", prompts: message.prompts });
        break;
      }
      case "providers_changed": {
        dispatch({ type: "providers_changed", roster: message.roster });
        break;
      }
      case "blob_ok": {
        // Resolving the correlated promise IS the notification: the awaiting
        // `runUpload` records the done state (with this blob) on the staged
        // file. Nothing else in the app tracks uploads.
        const pending = this.uploads.get(message.client_id);
        if (pending) {
          pending.resolve(message.blob);
          this.uploads.delete(message.client_id);
        }
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
          const historyReq = this.historyRequest;
          if (historyReq?.clientId === message.client_id) {
            clearTimeout(historyReq.timer);
            this.historyRequest = null;
            historyReq.reject(new Error(message.detail));
          }
        } else {
          // An uncorrelated error that arrives AFTER authentication is a runtime
          // protocol error (the pre-auth reject path returned above). Surface it
          // as a visible inline banner in the standing conversation rather than swallowing it into the
          // console, so a failure the Owner should see isn't invisible.
          setProtocolError(message.detail);
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
      this.socket.send(JSON.stringify(sendMessageFrame(pending)));
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
  blobBase = httpBaseFromWs(url);
  client = new HirselWsClient(url, token, handlers);
  client.connect();
  return client;
}

export function getClient(): HirselWsClient | null {
  return client;
}

export type { HirselWsClient };
