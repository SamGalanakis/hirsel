import type { ChatMessage, TaskFocus, ThreadTurnTimeline, ViewSpec } from "../protocol";
import type { ThreadSymbol, ThreadTint } from "./thread-symbols";
export type ThreadKind = "space" | "task";
export type ThreadIcon =
  | { kind: "symbol"; name: ThreadSymbol; tint: ThreadTint }
  | { kind: "image"; blob_id: string };
/** Where the Owner chose this Thread's next turn to run. Absent or null means
 * it inherits the configured default Native provider and model. */
export type ThreadExecutionTarget =
  | { kind: "native"; provider_id: string; model: string }
  | { kind: "cli"; agent: string; model: string; variant: string };
export interface Thread {
  id: number;
  kind: ThreadKind;
  parent_thread_id: number | null;
  pinned_at: string | null;
  title: string;
  icon: ThreadIcon | null;
  showcased_artifact_id: number | null;
  description: string;
  execution?: ThreadExecutionTarget | null;
  instrument: ViewSpec | ViewSpec[] | null;
  state: ThreadState;
  attention: "quiet" | "needs_owner";
  settled_at: string | null;
  archived_at: string | null;
  snoozed_until: string | null;
  read: boolean;
  created_at: string;
  updated_at: string;
  revision: number;
  running_turn: ThreadTurn | null;
  queued_turn_count: number;
  last_finished_turn: ThreadTurn | null;
  last_activity_at: string;
}
export interface ThreadState {
  revision: number;
  headline: string;
  own_headline: string;
  findings: string[];
  artifact_ids: number[];
  checkpoint_at: string | null;
  steering_revision: number;
}
export interface ThreadTurn {
  id: number;
  requester_thread_id: number | null;
  requester_turn_id: number | null;
  thread_id: number;
  owner_message_id: number | null;
  agent_message_id: number | null;
  state: "queued" | "running" | "completed" | "failed" | "cancelled" | "interrupted";
  accepted_at: string;
  started_at: string | null;
  finished_at: string | null;
}
export interface ThreadActivity {
  id: number;
  artifact_ids: number[];
  thread_id: number;
  turn_id: number | null;
  kind: string;
  data: unknown;
  ts: string;
}
export interface ThreadChange {
  change_id: number;
  thread_id: number;
  thread_title: string;
  state_revision: number;
  before_headline: string;
  after_headline: string;
  cause: string;
  source_space_id: number;
  source_space_title: string;
  created_at: string;
}
export interface ThreadChangeDigest { through_change_id: number; changes: ThreadChange[]; has_more: boolean }
export interface ThreadTurnContext {
  turn_id: number;
  context: { changes?: ThreadChangeDigest; [key: string]: unknown };
  through_change_id: number;
  consumed_at: string | null;
}
export type ThreadEffectKind = "created" | "sent_to" | "delegated" | "read" | "edited" | "refused";
export type ThreadEffectTarget = { kind: "thread"; thread_id: number } | { kind: "artifact"; artifact_id: number } | { kind: "root" };
export interface ThreadEffectRefusal { reason: string; grant_summary: string; detail: string }
export interface ThreadEffectReceipt {
  id: number; turn_id: number; operation_id: string; effect_index: number; tool: string;
  effect: ThreadEffectKind; target: ThreadEffectTarget; target_turn_id: number | null;
  request_client_id: string | null; refusal: ThreadEffectRefusal | null; created_at: string;
}
export type EffectAction =
  | { kind: "open"; target: ThreadEffectTarget }
  | { kind: "archive"; thread_id: number }
  | { kind: "cancel_queued"; thread_id: number; turn_id: number }
  | { kind: "stop"; thread_id: number; turn_id: number };
export interface ThreadEffect { receipt: ThreadEffectReceipt; actions: EffectAction[] }
export type RelatedTarget = { kind: "url"; url: string } | { kind: "thread"; history_id: string; thread_id: number };
export interface ThreadRelatedItem {
  id: number;
  thread_id: number;
  target: RelatedTarget;
  title: string | null;
  created_at: string;
}
export type ThreadGrantSource = { kind: "owner" } | { kind: "thread"; thread_id: number };
/** What one grant widens a Thread's reach to: one Thread and its subtree, or
 * the root — every Thread in the history, including ones created later. */
export type ThreadGrantTarget = { kind: "thread"; thread_id: number; title: string; thread_kind: ThreadKind } | { kind: "root" };
/** A reach target as an op names it: a Thread ID, or the literal "root". */
export type ReachTarget = number | "root";
/** One durable widening of a Thread's reach beyond self + descendants. */
export interface ThreadGrant {
  thread_id: number;
  target: ThreadGrantTarget;
  granted_by: ThreadGrantSource;
  granted_at: string;
  note: string | null;
}
export interface ThreadDetail {
  related_items: ThreadRelatedItem[];
  grants: ThreadGrant[];
  brief: { text: string; artifact_ids: number[] };
  thread: Thread;
  messages: ChatMessage[];
  turns: ThreadTurn[];
  effects?: ThreadEffect[];
  next_effects_before?: number | null;
  turn_timelines: ThreadTurnTimeline[];
  activities: ThreadActivity[];
  accepted_context?: ThreadTurnContext | null;
  has_more: boolean;
}
export type ThreadServerMessage =
  | { type: "thread_related_changed"; client_id: string | null; history_id: string; thread_id: number; revision: number; items: ThreadRelatedItem[] }
  | { type: "thread_grants_changed"; client_id: string | null; history_id: string; thread_id: number; revision: number; grants: ThreadGrant[] }
  | { type: "thread_effects_changed"; history_id: string; thread_id: number; turn_id: number; effects: ThreadEffect[] }
  | { type: "thread_upsert"; thread: Thread }
  | { type: "thread_created"; client_id: string; thread: Thread }
  | { type: "thread_action_applied"; client_id: string; history_id: string; thread_id: number }
  | { type: "thread_turn_cancellation_applied"; client_id: string; history_id: string; thread_id: number; turn_id: number }
  | { type: "thread_opened"; client_id: string; detail: ThreadDetail }
  | { type: "thread_activity"; activity: ThreadActivity }
  | { type: "thread_turn"; turn: ThreadTurn };
export type ThreadClientMessage =
  | { type: "add_thread_related"; client_id: string; history_id: string; thread_id: number; target: RelatedTarget; title: string | null }
  | { type: "remove_thread_related"; client_id: string; history_id: string; thread_id: number; item_id: number }
  | { type: "grant_thread_reach"; client_id: string; history_id: string; thread_id: number; target: ReachTarget; note: string | null }
  | { type: "revoke_thread_reach"; client_id: string; history_id: string; thread_id: number; target: ReachTarget }
  | { type: "create_thread"; client_id: string; history_id: string; title: string; kind: ThreadKind; parent_thread_id: number | null }
  | { type: "ensure_home_project"; client_id: string; history_id: string }
  | { type: "open_thread"; client_id: string; thread_id: number; before_id: number | null; effects_before?: number | null }
  | { type: "send_thread_message"; client_id: string; history_id: string; thread_id: number; body: string; focus?: TaskFocus; attachments: string[]; mentions: number[]; artifact_ids: number[]; mode: "send" | "next_turn" }
  | { type: "thread_action"; client_id: string; history_id: string; thread_id: number; action: string; data: unknown; expected_revision?: number }
  | { type: "cancel_thread_turn"; client_id: string; history_id: string; thread_id: number; turn_id: number; expected_state: ThreadTurn["state"] };
