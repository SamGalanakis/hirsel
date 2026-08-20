import type {
  AgentActivityState,
  Blob,
  ChatMessage,
  EventItem,
  EventUpsertMsg,
  HelloOkMsg,
  ModelSelection,
  ModelSnapshot,
  MessagesMsg,
  MsgMsg,
  ProcessInfo,
  ProcessUpsertMsg,
  SendMode,
  SubagentModelCatalog,
  TurnEvent,
  TurnEventMsg,
  ViewInstance,
  ViewRemovedMsg,
  ViewUpsertMsg,
} from "../protocol";

/** A conversation message as rendered locally. `pending`/`clientId`/`mode`/`failed`
 * only exist on (or are carried through from) optimistic entries created by
 * this client. `clientId` and `mode` are preserved across reconciliation so a
 * next_turn bubble can still be cancelled (cancel_queued) after the host echo. */
export type DisplayMessage = ChatMessage & {
  pending?: boolean;
  failed?: boolean;
  clientId?: string;
  mode?: SendMode;
};

/** An outgoing `send_message` this client has emitted but not yet seen
 * reconciled via a server `msg` echo. Kept so reconnect can resend with the
 * same `client_id` (host dedupes) and so we know what to reconcile against. */
export interface PendingSend {
  clientId: string;
  body: string;
  ref: number | null;
  // Omitted (not just defaulted) when empty/"send" so the common case keeps the
  // original {clientId, body, ref} shape the store snapshots and tests encode.
  attachments?: string[];
  mode?: SendMode;
  /** Legacy wire ids for @-mentioned Tasks, so a reconnect resend carries them
   * too. Omitted when empty (keeps the common-case shape). */
  mentions?: number[];
}

export type ConnectionStatus = "connecting" | "connected" | "reconnecting";

/** One Event's optimistic local assertions, each field the value the Owner's
 * gesture claims the host will shortly commit. An absent field asserts nothing
 * (the wire truth shows through). `snoozedUntil: null` is a real assertion —
 * "not snoozed" — as distinct from the field being absent. */
export interface EventOverride {
  /** Asserted `status !== "open"` (decide flips it true, reopen/undo false). */
  decided?: boolean;
  /** Asserted `archived === true`. */
  archived?: boolean;
  /** Asserted `read === true` (awareness auto-read on pass). */
  read?: boolean;
  /** Asserted `snoozed_until` (an RFC3339 instant, or `null` for un-snoozed). */
  snoozedUntil?: string | null;
}

export interface AgentActivity {
  state: AgentActivityState;
  text: string | null;
}

/** Per-file upload state driving the composer attachment chips (v1.1). Purely
 * the state machine + resolved blob; the raw File/preview object-URL live in
 * the Composer, keyed by the same `clientId`. */
export type UploadState = "uploading" | "done" | "error";

export interface Upload {
  clientId: string;
  name: string;
  size: number;
  mime: string;
  state: UploadState;
  blobId?: string; // set once blob_ok correlates
}

/** A single ordered timeline event from the running turn (v1.5), carrying its
 * wire `seq` alongside the tagged event body. Accumulated in `seq` order while
 * the Agent is thinking; folded into a rendered timeline by `buildTimeline`. */
export interface TimelineEvent {
  seq: number;
  event: TurnEvent;
  /** Client-side arrival timestamp (`Date.now()` at reduce time), stamped when
   * the event is first admitted. Lets the timeline show a per-tool duration
   * (tool_done.at − tool_start.at) and a running-turn elapsed reading without
   * any wire timing field. Optional so hand-constructed events (tests) and any
   * pre-existing replayed data render fine with no duration. */
  at?: number;
}

export interface AppState {
  messages: DisplayMessage[];
  /** Whether the Host reports more committed rows before the loaded range. */
  hasEarlierMessages: boolean;
  /** The bounded range evicted newer rows while the Owner was reading history. */
  hasLaterMessages: boolean;
  /** Product Tasks, carried as typed Events on the compatibility wire. Seeded
   * from `hello_ok.events` (authoritative on reconnect) and kept
   * current by `event_upsert`. Held as an ordered array so the store can
   * `reconcile` it keyed by `id` like messages. */
  events: EventItem[];
  /** The ONE optimistic layer over `events`, keyed by event id (R6-1). Every
   * local Event gesture — decide/undecide, archive/unarchive, auto-read,
   * snooze/unsnooze — records the value it ASSERTS here; `events` itself stays
   * the untouched wire truth so reconciliation always has something honest to
   * reconcile against. Read through `projectEvents` (store: `effectiveEvents()`),
   * never directly by a surface.
   *
   * One settle rule governs the whole record: an assertion is dropped exactly
   * when the wire truth already carries the value it asserts — applied at write
   * time (so a no-op gesture leaves no residue), on every `event_upsert`, and on
   * a `hello_ok` resync (where an event the snapshot no longer carries drops its
   * whole entry). Bounded. */
  eventOverrides: Record<number, EventOverride>;
  agentActivity: AgentActivity;
  connection: ConnectionStatus;
  lastSeenMsgId: number | null;
  /** Host build identity from the last `hello_ok` (crate version + git sha),
   * shown in Settings → About. `null` until a host that reports it connects
   * (older hosts omit the field; About shows "Not reported"). */
  hostVersion: string | null;
  /** The main agent's model snapshot (current selection + everything
   * selectable). Seeded from `hello_ok.model`; `current` is patched by
   * `model_changed`. `null` until a host that reports it connects (older hosts
   * omit the field; the Models control hides itself). */
  model: ModelSnapshot | null;
  /** The sub-agent model catalog, grouped by provider. Seeded from
   * `hello_ok.subagent_models` and replaced wholesale by
   * `subagent_models_changed`. `null` on older hosts (the control hides). */
  subagentModels: SubagentModelCatalog | null;
  pendingSends: PendingSend[];
  uploads: Upload[];
  /** v1.4: host-tracked background processes (sub-agents, monitors). Seeded by
   * hello_ok.processes and kept current by process_upsert. */
  processes: ProcessInfo[];
  /** v1.5: ordered events for the turn in progress, kept sorted by `seq`.
   * Ephemeral: cleared on turn commit (agent msg / agent_activity idle) and
   * frozen into `turnDetails` on an agent commit. */
  turnEvents: TimelineEvent[];
  /** The timeline of the turn that just ended, parked here when the turn went
   * idle before its committing agent message arrived. The Host publishes the
   * `agent_activity: idle` boundary from the observation bridge as soon as the
   * session commits, but broadcasts the committed `msg` afterwards from the
   * turn pump — so idle routinely precedes the message it belongs to. Parking
   * (rather than dropping) the events on idle is what lets the commit still
   * freeze them into `turnDetails`. Consumed by the next agent commit and
   * discarded as soon as a new turn starts streaming. */
  lastTurnEvents: TimelineEvent[];
  /** v1.5: finished-turn timelines kept in session memory, keyed by the id of
   * the agent message that committed the turn (the "turn details" affordance).
   * Not persisted — gone after reload. Bounded to the most recent turns. */
  turnDetails: Record<number, TimelineEvent[]>;
  /** Host ids tombstoned by msg_removed. A cancelled queued message can have
   * its removal race its own echo; keeping the id here means a late echo is
   * dropped instead of re-materializing the bubble. Bounded. */
  removedIds: number[];
  /** Generative-UI tier: active host-authored views keyed by `instance_id`.
   * Seeded from `hello_ok.views` (authoritative on reconnect), upserted by
   * `view_upsert` (an update in place re-sends the same id), dropped by
   * `view_removed`. `spec` is always the resolved concrete catalog tree; the
   * client never resolves templates/bindings. Held as an ordered array so the
   * store can `reconcile` it keyed by `instance_id` like messages. */
  views: ViewInstance[];
}

export type Action =
  | { type: "hello_ok"; payload: HelloOkMsg }
  | { type: "msg"; payload: MsgMsg }
  | { type: "messages_page"; payload: MessagesMsg; placement: "earlier" | "latest" }
  | { type: "msg_removed"; id: number }
  | { type: "agent_activity"; payload: { state: AgentActivityState; text: string | null } }
  | { type: "process_upsert"; payload: ProcessUpsertMsg }
  | { type: "turn_event"; payload: TurnEventMsg }
  | { type: "event_upsert"; payload: EventUpsertMsg }
  | { type: "event_decide_local"; eventId: number }
  | { type: "event_undecide_local"; eventId: number }
  | { type: "event_read_local"; eventId: number }
  | { type: "event_archive_local"; eventId: number }
  | { type: "event_unarchive_local"; eventId: number }
  | { type: "event_snooze_local"; eventId: number; until: string }
  | { type: "event_unsnooze_local"; eventId: number }
  | {
      type: "send_local";
      localId: number;
      clientId: string;
      body: string;
      ref: number | null;
      ts: string;
      attachments?: Blob[];
      mode?: SendMode;
      mentions?: number[];
    }
  | { type: "send_failed"; clientId: string }
  | { type: "send_retry"; clientId: string }
  | { type: "upload_start"; clientId: string; name: string; size: number; mime: string }
  | { type: "blob_ok"; clientId: string; blob: Blob }
  | { type: "upload_error"; clientId: string }
  | { type: "upload_retry"; clientId: string }
  | { type: "upload_remove"; clientId: string }
  | { type: "uploads_clear" }
  | { type: "connection_status"; status: ConnectionStatus }
  // ---- Generative-UI tier (view templates) ----
  | { type: "view_upsert"; payload: ViewUpsertMsg }
  | { type: "view_removed"; payload: ViewRemovedMsg }
  // ---- Model configuration ----
  | { type: "model_changed"; current: ModelSelection }
  | { type: "subagent_models_changed"; catalog: SubagentModelCatalog };

export function initialState(): AppState {
  return {
    messages: [],
    hasEarlierMessages: false,
    hasLaterMessages: false,
    events: [],
    eventOverrides: {},
    agentActivity: { state: "idle", text: null },
    connection: "connecting",
    lastSeenMsgId: null,
    hostVersion: null,
    model: null,
    subagentModels: null,
    pendingSends: [],
    uploads: [],
    processes: [],
    turnEvents: [],
    lastTurnEvents: [],
    turnDetails: {},
    removedIds: [],
    views: [],
  };
}
