import type { ChatMessage, ViewSpec } from "../protocol";
export interface Thread {
  id: number;
  parent_thread_id: number | null;
  pinned_at: string | null;
  title: string;
  icon: string | null;
  showcased_artifact_id: number | null;
  description: string;
  instrument: ViewSpec | ViewSpec[] | null;
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
export interface ThreadTurn {
  id: number;
  requester_thread_id: number | null;
  requester_turn_id: number | null;
  thread_id: number;
  owner_message_id: number | null;
  agent_message_id: number | null;
  state: "queued" | "running" | "completed" | "failed" | "cancelled" | "interrupted";
  started_at: string;
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
export type RelatedTarget = { kind: "url"; url: string } | { kind: "thread"; history_id: string; thread_id: number };
export interface ThreadRelatedItem {
  id: number;
  thread_id: number;
  target: RelatedTarget;
  title: string | null;
  created_at: string;
}
export interface ThreadDetail {
  related_items: ThreadRelatedItem[];
  brief: { text: string; artifact_ids: number[] };
  thread: Thread;
  messages: ChatMessage[];
  turns: ThreadTurn[];
  activities: ThreadActivity[];
  has_more: boolean;
}
export type ThreadServerMessage =
  | { type: "thread_related_changed"; client_id: string | null; history_id: string; thread_id: number; revision: number; items: ThreadRelatedItem[] }
  | { type: "thread_upsert"; thread: Thread }
  | { type: "thread_created"; client_id: string; thread: Thread }
  | { type: "thread_action_applied"; client_id: string; history_id: string; thread_id: number }
  | { type: "thread_opened"; client_id: string; detail: ThreadDetail }
  | { type: "thread_activity"; activity: ThreadActivity }
  | { type: "thread_turn"; turn: ThreadTurn };
export type ThreadClientMessage =
  | { type: "add_thread_related"; client_id: string; history_id: string; thread_id: number; target: RelatedTarget; title: string | null }
  | { type: "remove_thread_related"; client_id: string; history_id: string; thread_id: number; item_id: number }
  | { type: "create_thread"; client_id: string; history_id: string; title: string; parent_thread_id: number | null }
  | { type: "open_thread"; client_id: string; thread_id: number; before_id: number | null }
  | { type: "send_thread_message"; client_id: string; history_id: string; thread_id: number; body: string; attachments: string[]; mentions: number[]; artifact_ids: number[]; mode: "send" | "next_turn" }
  | { type: "thread_action"; client_id: string; history_id: string; thread_id: number; action: string; data: unknown; expected_revision?: number };
