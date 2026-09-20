import { createStore } from "solid-js";

import type { TaskFocus } from "../protocol";
import type { Thread } from "../threads/types";
import { ancestorsIn, threadIndex } from "../threads/tree";

interface ProjectState {
  /** The top-level Space whose conversation receives project-chat messages. */
  projectRecipientId: number | null;
  /** The bounded Task snapshot explicitly staged for the next project message. */
  taskFocus: TaskFocus | null;
  /** The worker whose own conversation the Owner has stepped into. */
  workerPairingId: number | null;
}

export const [projectState, setProjectState] = createStore<ProjectState>({
  projectRecipientId: null,
  taskFocus: null,
  workerPairingId: null,
});

export function resetProjects(): void {
  setProjectState(draft => { Object.assign(draft, { projectRecipientId: null, taskFocus: null, workerPairingId: null }); });
}

export function projectForThread(threads: Thread[], threadId: number): Thread | null {
  const index = threadIndex(threads);
  const thread = index.get(threadId);
  if (!thread) return null;
  const path = [...ancestorsIn(index, threadId), thread];
  return path.find(candidate => candidate.parent_thread_id === null && candidate.kind === "space") ?? null;
}

export function enterProject(projectId: number, preserveStagedFocus = false): void {
  setProjectState(draft => { Object.assign(draft, {
    projectRecipientId: projectId,
    taskFocus: preserveStagedFocus && draft.projectRecipientId === projectId ? draft.taskFocus : null,
    workerPairingId: null,
  }); });
}

export function stepIntoWorker(threads: Thread[], threadId: number): void {
  const project = projectForThread(threads, threadId);
  setProjectState(draft => { Object.assign(draft, {
    projectRecipientId: project?.id ?? null,
    taskFocus: null,
    workerPairingId: threadId,
  }); });
}

function boundedUtf8(value: string, maxBytes: number): string {
  const encoder = new TextEncoder();
  const bytes = encoder.encode(value);
  if (bytes.length <= maxBytes) return value;
  let end = maxBytes;
  while (end > 0) {
    try { return `${new TextDecoder("utf-8", { fatal: true }).decode(bytes.slice(0, end))}…`; }
    catch { end -= 1; }
  }
  return "…";
}

function instrumentSummary(instrument: Thread["instrument"]): string | null {
  if (instrument === null) return null;
  const summary = JSON.stringify(instrument);
  return boundedUtf8(summary, 2_000);
}

export function stageTaskFocus(threads: Thread[], threadId: number, brief: string, recipientId?: number): number | null {
  const task = threads.find(candidate => candidate.id === threadId && candidate.kind === "task");
  const project = recipientId === undefined
    ? task ? projectForThread(threads, threadId) : null
    : threads.find(candidate => candidate.id === recipientId && candidate.kind === "space" && candidate.parent_thread_id === null) ?? null;
  if (!task || !project) return null;
  setProjectState(draft => { Object.assign(draft, {
    projectRecipientId: project.id,
    workerPairingId: null,
    taskFocus: {
      task_thread_id: task.id,
      snapshot: {
        title: task.title,
        brief: boundedUtf8(brief.trim() || task.description, 8_000),
        instrument_summary: instrumentSummary(task.instrument),
      },
    },
  }); });
  return project.id;
}

export function consumeTaskFocus(): void {
  setProjectState(draft => { draft.taskFocus = null; });
}
