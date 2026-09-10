import type { Thread, ThreadClientMessage, ThreadServerMessage } from "./threads/types";
import type { ArtifactClientMessage, ArtifactServerMessage } from "./artifacts/types";
// Current-only mirror of app/PROTOCOL.md and hirsel-proto.
// Transport: WebSocket, JSON text frames, one message per frame.

export type Author = "owner" | "agent";

/** A stored attachment (v1.1). CONTENT is fetched out-of-band from
 * a signed blob URL; this record is only the metadata carried on the
 * wire. */
export interface Blob {
  id: string; // uuid
  name: string;
  mime: string;
  size: number; // u64, decoded byte size
}

/** A single tool the Agent invoked during the turn that produced a committed
 * agent message, keyed by the same canonical ID as its streamed events. */
export interface ToolCall {
  id: string;
  name: string;
  ok: boolean;
}

export interface ChatMessage {
  artifact_ids?: number[];
  thread_id: number;
  client_id?: string;
  id: number; // u64, monotonic, host-assigned
  author: Author;
  body: string; // markdown
  ref: number | null; // id of the chat message this replies to
  ts: string; // RFC3339
  attachments?: Blob[]; // v1.1, default []
  /** Thread IDs cited by this message; citations never change ownership. */
  mentions?: number[];
  /** v1.4: tools invoked in the turn that committed this (agent) message.
   * Optional on the wire; absent/empty renders no footer chip. */
  tool_calls?: ToolCall[];
}

/** A host-tracked monitor probe the Agent has running (v1.4). Surfaced in the
 * Processes tab. */
export type ProcessKind = "monitor";

/** `running` is the only non-terminal state; the rest are terminal. `failed` /
 * `abandoned` get a warning tint in the UI. */
export type ProcessState = "running" | "done" | "failed" | "cancelled" | "abandoned";

export interface ProcessInfo {
  thread_id: number;
  id: string;
  kind: ProcessKind;
  label: string;
  /** Nullable wire metadata retained for current server/native clients. */
  agent: string | null;
  model: string | null;
  state: ProcessState;
  started_ts: string; // RFC3339
  last_event_ts: string; // RFC3339, drives newest-activity-first ordering
  summary: string | null; // latest progress line, single-line truncated in UI
}

/** Generative-UI tier (view templates): a resolved, concrete component tree the
 * host sends for the client to render natively. The client NEVER resolves
 * templates or bindings — `spec` is always the finished JSON tree of catalog
 * components (see templates/CATALOG.md). Typed loosely on the wire (a `type`
 * discriminator plus arbitrary resolved props/children); the renderer narrows
 * per catalog component and degrades gracefully on anything it doesn't know. */
export interface ViewSpec {
  type: string;
  [prop: string]: unknown;
}

/** One active view instance. Keyed by `instance_id`; an update in place is just
 * a re-`view_upsert` of the same id. */
export interface ViewInstance {
  thread_id: number;
  instance_id: string;
  spec: ViewSpec;
}

// ---- Model configuration (main agent + sub-agent catalog) ----

/** The main agent's current model + reasoning variant. `variant` is one of the
 * chosen model's `variants` (e.g. "low" … "max"). */
export interface ModelSelection {
  id: string;
  variant: string;
}

/** A model the main agent may run as, plus its selectable reasoning variants and
 * the variant to fall back to when the model is chosen without one. */
export interface AvailableModel {
  id: string;
  label: string;
  variants: string[];
  default_variant: string;
}

/** The main-agent model snapshot carried on `hello_ok` and kept current by
 * `model_changed`: what's selected now, and everything selectable. */
export interface ModelSnapshot {
  current: ModelSelection;
  available: AvailableModel[];
  /** The provider instance this agent runs on. Null when no provider is configured. */
  provider_id?: string;
  /** True when the selected provider takes a free-text model id: `available`
   * is then empty and `current.id` is whatever the Owner typed. */
  free_text_model?: boolean;
}

/** One editable prompt as the Host actually resolves it. `is_default` means
 * no non-empty override is stored; `text` still carries the bundled body so
 * Settings always edits the effective value. */
export interface PromptDoc {
  text: string;
  is_default: boolean;
}

/** Persisted configuration for the ephemeral incoming-event triage fork. */
export interface ForkAgentConfig {
  current: ModelSelection;
  available: AvailableModel[];
  prompt: PromptDoc;
  /** The provider instance this agent runs on. Null when no provider is configured. */
  provider_id?: string;
  /** True when the selected provider takes a free-text model id: `available`
   * is then empty and `current.id` is whatever the Owner typed. */
  free_text_model?: boolean;
}

/** The complete prompt surface carried on `hello_ok` and replaced wholesale
 * by `prompts_changed`. `fork` is absent when the active provider has no
 * runtime-selectable model registry. */
export interface PromptSnapshot {
  agent: PromptDoc;
  fork?: ForkAgentConfig;
}

/** One model in the sub-agent catalog: `enabled` is the master availability
 * switch and `enabled_variants` is the independently selectable reasoning
 * allow-list. */
export interface SubagentModel {
  id: string;
  label: string;
  variants: string[];
  enabled_variants: string[];
  enabled: boolean;
}

/** The sub-agent models offered by one provider (e.g. Codex CLI, Claude Code
 * CLI), grouped under its display `label`. */
export interface SubagentProviderModels {
  provider: string;
  label: string;
  models: SubagentModel[];
}

/** The full sub-agent model catalog carried on `hello_ok` and replaced wholesale
 * by `subagent_models_changed`. */
export interface SubagentModelCatalog {
  providers: SubagentProviderModels[];
}

// ---- Provider roster ----

/** How a provider instance authenticates and what shape its model choice takes.
 * `codex` and `claude` are locally-detected OAuth credentials; every other
 * instance is an OpenAI-compatible endpoint with a base URL and an API key. */
export type ProviderKind = "codex" | "claude" | "openai_compatible";

/** The model controls an agent-selectable provider offers. */
export type ProviderSelection =
  | { mode: "curated"; main: AvailableModel[]; fork: AvailableModel[] }
  | { mode: "free_text" };

/** A stored secret as the wire is allowed to describe it. The full key never
 * leaves the host — presence and a short tail are the whole vocabulary. */
export interface MaskedSecret {
  present: boolean;
  tail?: string;
}

/** Whether the host can see the local credentials an OAuth provider needs. */
export interface DetectionStatus {
  detected: boolean;
  path: string;
  account_hint?: string;
  detail?: string;
}

/** One configured provider instance. */
export interface ProviderInstance {
  id: string;
  kind: ProviderKind;
  label: string;
  base_url?: string;
  api_key?: MaskedSecret;
  default_model?: string;
  detection?: DetectionStatus;
  /** Whether the main Agent and the fork may select it. Claude is false. */
  agent_selectable: boolean;
  /** The provider-specific model controls. Absent on providers that resident agents cannot select. */
  selection?: ProviderSelection;
  /** Built-in instances (codex, claude) are configured, never removed. */
  removable: boolean;
}

/** The whole roster, carried on `hello_ok` and replaced by `providers_changed`. */
export interface ProviderRoster {
  instances: ProviderInstance[];
  /** The provider the resident session actually booted on — a main-agent
   * provider change is stored at once but only takes effect on restart. */
  booted_provider_id?: string;
  /** Set when a stored provider choice could not be honoured at boot and the
   * host fell back to its environment default. Carries no key material. */
  boot_notice?: string;
}

/** Which resident agent a provider op addresses. */
export type AgentSlot = "main" | "fork";

// ---- Client -> server ----

export interface HelloMsg {
  type: "hello";
  auth: { static_token: string } | { device_token: string } | { pairing_code: { code: string; device_label: string } };
}

/** v1.2 send mode. "send" = plain Enter (Early Injection if a turn is active,
 * else normal ingress); "next_turn" = explicit queue (always held until the current turn
 * commits, lash Next Full Turn). Absent is treated as "send". */
export type SendMode = "send" | "next_turn";

/** v1.1: upload a file's bytes (base64) before referencing it from a
 * send_thread_message. Correlated to a blob_ok by `client_id`. */
export interface UploadBlobMsg {
  type: "upload_blob";
  client_id: string;
  name: string;
  mime: string;
  data_b64: string;
}

/** D9: request a short-lived signed URL to fetch a blob's bytes. Replaces the
 * removed `GET /blob/{id}?token=<owner-token>` scheme (the host now rejects the
 * raw-token query param). Correlated to a `blob_url` by `client_id`. */
export interface GetBlobUrlMsg {
  type: "get_blob_url";
  client_id: string;
  blob_id: string;
}

/** v1.2: cooperatively interrupt the active agent turn (Esc). No-op if idle. */
export interface CancelTurnMsg {
  history_id: string;
  thread_id: number;
  type: "cancel_turn";
}

/** v1.2: cancel a not-yet-claimed queued (next_turn) message. Host maps
 * `client_id` to its pending-input id; already-claimed → error. */
export interface CancelQueuedMsg {
  type: "cancel_queued";
  client_id: string;
}

/** Current host-authored View interaction; the Host handles its action payload. */
export interface ViewEventMsg {
  type: "view_event";
  instance_id: string;
  action: string;
  data: unknown;
}

/** Select the main agent's model + reasoning variant. The host applies it and
 * broadcasts the truth back as `model_changed`. */
export interface SetModelMsg {
  type: "set_model";
  provider_id: string;
  model_id: string;
  variant: string;
}

/** Update one sub-agent catalog model's master state and reasoning allow-list.
 * Carries the FULL row state so it's a complete upsert, not a diff; the host
 * applies it and broadcasts `subagent_models_changed`. */
export interface SetSubagentModelMsg {
  type: "set_subagent_model";
  provider: string;
  model_id: string;
  enabled: boolean;
  enabled_variants: string[];
}

/** Replace the main Agent's editable prompt body. Empty text removes the
 * override, restoring the bundled default from the next turn. */
export interface SetAgentPromptMsg {
  type: "set_agent_prompt";
  text: string;
}

/** Replace the fork's prompt body. Empty text restores its bundled default. */
export interface SetForkPromptMsg {
  type: "set_fork_prompt";
  text: string;
}

/** Select the fork model from the active provider's registry. */
export interface SetForkModelMsg {
  type: "set_fork_model";
  provider_id: string;
  model_id: string;
  variant: string;
}

/** Point one resident agent at a provider instance, seeding that provider's
 * default model + variant. The host stores it and broadcasts the truth back. */
export interface SetAgentProviderMsg {
  type: "set_agent_provider";
  agent: AgentSlot;
  provider_id: string;
}

/** Add an OpenAI-compatible provider instance. */
export interface AddProviderMsg {
  type: "add_provider";
  id: string;
  label: string;
  base_url: string;
  api_key: string;
  default_model: string;
}

/** Edit one instance. Omitted fields are unchanged; an `api_key` of `""`
 * clears the stored key. */
export interface UpdateProviderMsg {
  type: "update_provider";
  id: string;
  label?: string;
  base_url?: string;
  api_key?: string;
  default_model?: string;
}

/** Remove a removable instance. */
export interface RemoveProviderMsg {
  type: "remove_provider";
  id: string;
}

/** Re-probe an OAuth provider's local credentials. */
export interface RedetectProviderMsg {
  type: "redetect_provider";
  id: string;
}

export type ClientMessage =
  | ArtifactClientMessage
  | ThreadClientMessage
  | HelloMsg
  | UploadBlobMsg
  | GetBlobUrlMsg
  | CancelTurnMsg
  | CancelQueuedMsg
  | ViewEventMsg
  | SetModelMsg
  | SetSubagentModelMsg
  | SetAgentPromptMsg
  | SetForkPromptMsg
  | SetForkModelMsg
  | SetAgentProviderMsg
  | AddProviderMsg
  | UpdateProviderMsg
  | RemoveProviderMsg
  | RedetectProviderMsg;

// ---- Server -> client ----

export interface HelloOkMsg {
  type: "hello_ok";
  history_id: string;
  threads: Thread[];
  processes: ProcessInfo[];
  views: ViewInstance[];
  host_version: string;
  model: ModelSnapshot | null;
  subagent_models: SubagentModelCatalog | null;
  prompts: PromptSnapshot | null;
  providers: ProviderRoster | null;
}

export interface MsgMsg {
  type: "msg";
  message: ChatMessage;
}

export type AgentActivityState = "thinking" | "idle";

export interface AgentActivityMsg {
  turn_id: number;
  thread_id: number;
  type: "agent_activity";
  state: AgentActivityState;
  text: string | null;
}

/** v1.1: ack for an upload_blob, correlated by `client_id`. */
export interface BlobOkMsg {
  type: "blob_ok";
  client_id: string;
  blob: Blob;
}

/** D9: a short-lived, HMAC-signed URL for fetching a blob's bytes, answering
 * `get_blob_url` (correlated by `client_id`). `url` is host-relative
 * (`/blob/{id}?exp=&sig=`) — prefix the blob origin; `expires_at` is unix
 * seconds (≈5 min out). Superseded the removed `?token=` scheme. */
export interface BlobUrlMsg {
  type: "blob_url";
  client_id: string;
  blob_id: string;
  url: string;
  expires_at: number;
}

/** v1.2: tombstone for a cancelled queued message; clients drop the bubble. */
export interface MsgRemovedMsg {
  type: "msg_removed";
  id: number;
}

/** v1.4: full-process upsert broadcast on any state/summary change. */
export interface ProcessUpsertMsg {
  type: "process_upsert";
  process: ProcessInfo;
}

/** v1.5: one ordered event in the running turn's timeline. Tagged by `kind`.
 * Prose/reasoning carry markdown deltas that accumulate into the current
 * block/run; tool_start opens a row that tool_done (matched by `id`) resolves.
 * `tool_done` carries its own `name` too, so an orphan done (no matching start,
 * e.g. a reconnect mid-turn) still renders a labelled row. Host `summary`
 * strings are clean one-liners (no raw JSON).
 * `code_start`/`code_done` pair the same way for one Agent program cell, except
 * `code_start.code` is the FULL source (rendered as code, not condensed);
 * `truncated` marks the rare cell clipped at the host's 64 KiB safety cap. */
export type TurnEvent =
  | { kind: "prose"; text: string }
  | { kind: "reasoning"; text: string }
  | { kind: "tool_start"; id: string; name: string; summary: string | null; input: TurnEventPayload | null }
  | { kind: "tool_done"; id: string; name: string; ok: boolean; summary: string | null; result: TurnEventPayload | null }
  | { kind: "code_start"; id: string; language: string; code: string; truncated: boolean }
  | { kind: "code_done"; id: string; ok: boolean; summary: string | null };

export interface TurnEventPayload { text: string; truncated: boolean }
export interface TurnEventRecord { seq: number; event: TurnEvent }
export interface ThreadTurnTimeline { turn_id: number; events: TurnEventRecord[] }

/** One durably appended timeline event streamed after its commit. `seq`
 * strictly orders events within a turn; redelivery is idempotent. */
export interface TurnEventMsg extends TurnEventRecord {
  turn_id: number;
  thread_id: number;
  type: "turn_event";
}

export interface ErrorMsg {
  type: "error";
  detail: string;
  /** Optional correlation id echoed for request failures, including Thread
   * actions, so the client can settle the exact owning operation. */
  client_id?: string;
}

/** Generative-UI tier: seed or update a view in place, keyed by `instance_id`.
 * An update in place is a re-send of the same id with a new resolved `spec`. */
export interface ViewUpsertMsg {
  thread_id: number;
  type: "view_upsert";
  instance_id: string;
  spec: ViewSpec;
}

/** Generative-UI tier: drop the view with this `instance_id`. */
export interface ViewRemovedMsg {
  type: "view_removed";
  instance_id: string;
}

/** The main agent's model surface changed (in response to a `set_model` or a
 * `set_agent_provider`, or a host-side change). Carries the FULL replacement
 * snapshot, because a provider change reshapes the control itself: a curated
 * registry with a reasoning ladder and a free-text model id are two different
 * questions, and neither can be derived from a bare selection. */
export interface ModelChangedMsg {
  type: "model_changed";
  model: ModelSnapshot;
}

/** The sub-agent model catalog changed (in response to a `set_subagent_model`,
 * or a host-side change). Carries the full replacement catalog. */
export interface SubagentModelsChangedMsg {
  type: "subagent_models_changed";
  catalog: SubagentModelCatalog;
}

/** The prompt surface changed through Settings. Full replacement keeps model
 * and prompt controls synchronized by one broadcast. */
export interface PromptsChangedMsg {
  type: "prompts_changed";
  prompts: PromptSnapshot;
}

/** The provider roster after an accepted edit — the whole roster, because one
 * edit can change another instance's derived state. */
export interface ProvidersChangedMsg {
  type: "providers_changed";
  roster: ProviderRoster;
}

/** Plugin tier: an unsolicited push from one Host-side plugin to its own
 * browser-side UI bundle. The app never interprets `data` — it routes the frame
 * to the handlers that plugin registered for `topic` via `api.onPush` and does
 * nothing else. A frame for an unknown plugin or an unsubscribed topic is
 * dropped silently; plugin data is never app state. */
export interface PluginPushMsg {
  type: "plugin_push";
  plugin: string;
  topic: string;
  data: unknown;
}

export type ServerMessage =
  | ArtifactServerMessage
  | ThreadServerMessage
  | HelloOkMsg
  | MsgMsg
  | AgentActivityMsg
  | BlobOkMsg
  | BlobUrlMsg
  | MsgRemovedMsg
  | ProcessUpsertMsg
  | TurnEventMsg
  | ErrorMsg
  | ViewUpsertMsg
  | ViewRemovedMsg
  | ModelChangedMsg
  | SubagentModelsChangedMsg
  | PromptsChangedMsg
  | ProvidersChangedMsg
  | PluginPushMsg;
