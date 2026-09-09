import type { ChatMessage, ViewSpec } from "../protocol";
export interface Thread {
  id: number;
  title: string;
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
  thread_id: number;
  owner_message_id: number | null;
  agent_message_id: number | null;
  state: "queued" | "running" | "completed" | "failed" | "cancelled" | "interrupted";
  started_at: string;
  finished_at: string | null;
}
export interface ThreadActivity {
  id: number;
  thread_id: number;
  turn_id: number | null;
  kind: string;
  data: unknown;
  ts: string;
}
export interface ThreadDetail {
  thread: Thread;
  messages: ChatMessage[];
  turns: ThreadTurn[];
  activities: ThreadActivity[];
  has_more: boolean;
}
export type ThreadServerMessage =
  | { type: "thread_upsert"; thread: Thread }
  | { type: "thread_created"; client_id: string; thread: Thread }
  | { type: "thread_opened"; client_id: string; detail: ThreadDetail }
  | { type: "thread_activity"; activity: ThreadActivity }
  | { type: "thread_turn"; turn: ThreadTurn };
export type ThreadClientMessage =
  | { type: "create_thread"; client_id: string; title: string }
  | { type: "open_thread"; client_id: string; thread_id: number; before_id: number | null }
  | { type: "send_thread_message"; client_id: string; thread_id: number; body: string; attachments: string[]; mentions: number[]; mode: "send" | "next_turn" }
  | { type: "thread_action"; thread_id: number; action: string; data: unknown; expected_revision?: number };
