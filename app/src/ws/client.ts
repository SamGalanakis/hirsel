import { attachRelatedTransport, disconnectRelated, handleRelatedMessage, resetRelated, trackRelatedRead } from "../related/store";
import { acceptHistory } from "../lib/history";
import { attachArtifactTransport, disconnectArtifacts, handleArtifactMessage, resetArtifacts } from "../artifacts/store";
import { attachThreadTransport, disconnectThreads, handleThreadMessage, resetThreads } from "../threads/store";
// Single WebSocket client module: connect, hello/hello_ok, reconnect with
// exponential backoff, offline outgoing queue flushed on reconnect using
// stable client_ids so the host can dedupe resends. Also owns the v1.1 blob
// upload correlation and the v1.2 send-mode / cancel frames.
import type {
  AgentSlot,
  Blob,
  ClientMessage,
  ServerMessage,
} from "../protocol";
import { httpBaseFromWs } from "../lib/endpoint";
import { deliverPluginPush } from "../plugins/registry";
import { dispatch, setProtocolError } from "../store/store";
import { jitteredDelayMs } from "./backoff";

const TOKEN_KEY = "hirsel.token";


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
}

export function makeClientId(): string {
  return crypto.randomUUID();
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
  private authenticated = false;
  /** Unresolved upload_blob promises, keyed by their client_id. */
  private uploads = new Map<string, { resolve: (b: Blob) => void; reject: (e: Error) => void }>();
  /** Unresolved get_blob_url promises, keyed by their client_id (D9). */
  private blobUrlReqs = new Map<string, { resolve: (url: string) => void; reject: (e: Error) => void }>();
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
    disconnectThreads();
    disconnectRelated();
    disconnectArtifacts();
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    this.clearRequests("Connection closed.");
    this.socket?.close();
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

  cancelTurn(historyId: string, threadId: number): void {
    this.sendFrame({ type: "cancel_turn", history_id: historyId, thread_id: threadId });
  }

  cancelQueued(clientId: string): void {
    this.sendFrame({ type: "cancel_queued", client_id: clientId });
  }

  // ---- Generative-UI tier (view templates) ----

  /** Deliver a current View interaction; the Host owns its resulting state. */
  sendViewEvent(instanceId: string, action: string, data: unknown): void {
    this.enqueue({ type: "view_event", instance_id: instanceId, action, data });
  }

  // ---- Model configuration ----

  /** Select the main agent's model + reasoning variant. Enqueued so a tap right
   * as the socket blips still lands once reconnected; the UI shows a brief
   * pending state and settles on the `model_changed` broadcast (no permanent
   * optimistic divergence — the store is only written by the broadcast). */
  setModel(providerId: string, modelId: string, variant: string): void {
    this.enqueue({ type: "set_model", provider_id: providerId, model_id: modelId, variant });
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

  /** Update the native worker row's full state (enabled + model override). An
   * empty or absent model clears the override. Settles on
   * `subagent_models_changed`. */
  setNativeWorker(enabled: boolean, model?: string): void {
    const trimmed = model?.trim();
    this.enqueue({
      type: "set_native_worker",
      enabled,
      ...(trimmed ? { model: trimmed } : {}),
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
  setForkModel(providerId: string, modelId: string, variant: string): void {
    this.enqueue({
      type: "set_fork_model",
      provider_id: providerId,
      model_id: modelId,
      variant,
    });
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

  private enqueue(frame: ClientMessage): void {
    if (this.authenticated && this.socket && this.socket.readyState === WebSocket.OPEN) {
      this.socket.send(JSON.stringify(frame));
    } else {
      this.outbox.push(frame);
    }
  }

  /** Best-effort immediate send; dropped if the socket is not open right now
   * (Thread send durability comes from the Thread store). */
  private sendFrame(frame: ClientMessage): void {
    if (this.authenticated && this.socket && this.socket.readyState === WebSocket.OPEN) {
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
    this.authenticated = false;

    socket.addEventListener("open", () => {
      const hello: ClientMessage = {
        type: "hello",
        auth: { static_token: this.token },
      };
      socket.send(JSON.stringify(hello));
    });

    socket.addEventListener("message", (event) => {
      this.handleServerMessage(JSON.parse(event.data as string) as ServerMessage);
    });

    socket.addEventListener("close", (event) => {
      this.socket = null;
      this.authenticated = false;
      disconnectThreads();
    disconnectRelated();
    disconnectArtifacts();
      if (this.closedByClient) return;
      if (AUTH_REJECT_CODES.has((event as CloseEvent).code)) {
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
   * message for an explicit authentication close code. */
  private handleAuthReject(detail?: string): void {
    if (this.closedByClient) return; // already torn down (e.g. error then close)
    this.closedByClient = true;
    disconnectThreads();
    disconnectRelated();
    disconnectArtifacts(); // suppress any in-flight reconnect/close paths
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.clearRequests("Connection closed.");
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
    if (message.type === "hello_ok") {
      if (acceptHistory(message.history_id)) { this.clearRequests("History was reset. Start this request again."); resetThreads(); resetArtifacts(); resetRelated(); }
      this.authenticated = true;
      attachThreadTransport(frame => { trackRelatedRead(frame, message.history_id); this.sendFrame(frame); });
      attachRelatedTransport(frame => this.sendFrame(frame));
      attachArtifactTransport(frame => this.sendFrame(frame));
    }
    handleArtifactMessage(message);
    handleRelatedMessage(message);
    handleThreadMessage(message);
    switch (message.type) {
      case "hello_ok": {
        dispatch({ type: "hello_ok", payload: message });
        dispatch({ type: "connection_status", status: "connected" });
        this.reconnectAttempt = 0;
        this.flushOutbox();
        break;
      }
      case "process_upsert": {
        dispatch({ type: "process_upsert", payload: message });
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
        // code). Before the current socket has authenticated, that is an auth
        // rejection — act on it immediately (precise + instant) rather than
        // waiting for the socket close. A correlated error
        // (upload/blob) always has a client_id and is handled below; a global
        // error that arrives AFTER authentication is a normal runtime error.
        if (!this.authenticated && !message.client_id) {
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

  private clearRequests(detail: string): void {
    this.outbox = [];
    for (const request of this.uploads.values()) request.reject(new Error(detail));
    for (const request of this.blobUrlReqs.values()) request.reject(new Error(detail));
    this.uploads.clear(); this.blobUrlReqs.clear();
  }
  private flushOutbox(): void {
    if (!this.socket || this.socket.readyState !== WebSocket.OPEN) return;

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
