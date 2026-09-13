import type { ProcessState, ProcessInfo, ViewInstance } from "../protocol";
import type { Thread } from "../threads/types";
export function isProcessRunning(state: ProcessState): boolean {
  return state === "running" || state === "waiting";
}

/** Count backing the Processes utility badge: running processes only.
 * Deliberately independent of Thread attention state and document.title. */
export function runningProcessCount(processes: ProcessInfo[]): number {
  return processes.filter((p) => isProcessRunning(p.state)).length;
}

export function scopedProcesses(
  processes: ProcessInfo[],
  threads: Thread[],
  focusedId: number | null,
): ProcessInfo[] {
  if (focusedId === null) return [];
  const visible = new Set([focusedId]);
  let changed = true;
  while (changed) {
    changed = false;
    for (const thread of threads) {
      if (thread.parent_thread_id !== null && visible.has(thread.parent_thread_id) && !visible.has(thread.id)) {
        visible.add(thread.id);
        changed = true;
      }
    }
  }
  return processes.filter(process => visible.has(process.thread_id));
}

// ---- Generative-UI tier (view templates) ----

/** Views on the shared Canvas surface, oldest-first (latest-upsert order).
 * The Canvas auto-surfaces the newest, i.e. the LAST of this list. */
export function canvasViews(views: ViewInstance[], threadId: number | null): ViewInstance[] {
  return views.filter((v) => v.thread_id === threadId);
}

/** Group processes into Running / Finished, each newest-activity-first
 * (`last_event_ts` desc, id as a stable tiebreak). One place so the tab list
 * and any future surfaces agree on ordering. */
export function partitionProcesses(processes: ProcessInfo[]): {
  running: ProcessInfo[];
  finished: ProcessInfo[];
} {
  const byActivity = (a: ProcessInfo, b: ProcessInfo) => {
    const d = Date.parse(b.last_event_ts) - Date.parse(a.last_event_ts);
    return d !== 0 ? d : a.id < b.id ? 1 : -1;
  };
  const running = processes.filter((p) => isProcessRunning(p.state)).sort(byActivity);
  const finished = processes.filter((p) => !isProcessRunning(p.state)).sort(byActivity);
  return { running, finished };
}
