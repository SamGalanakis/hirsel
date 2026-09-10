import type { ProcessState, ProcessInfo, ViewInstance } from "../protocol";
export function isProcessRunning(state: ProcessState): boolean {
  return state === "running";
}

/** Count backing the Processes utility badge: running processes only.
 * Deliberately independent of Thread attention state and document.title. */
export function runningProcessCount(processes: ProcessInfo[]): number {
  return processes.filter((p) => isProcessRunning(p.state)).length;
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
