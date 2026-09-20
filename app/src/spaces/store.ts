import { createStore } from "solid-js";

import type { Thread } from "../threads/types";
import { ancestorsIn, threadIndex } from "../threads/tree";

interface SpaceState {
  /** The Space whose conversation receives Space-chat messages. */
  spaceRecipientId: number | null;
  /** The worker whose own conversation the Owner has stepped into. */
  workerPairingId: number | null;
}

export const [spaceState, setSpaceState] = createStore<SpaceState>({
  spaceRecipientId: null,
  workerPairingId: null,
});

export function resetSpaces(): void {
  setSpaceState(draft => { Object.assign(draft, { spaceRecipientId: null, workerPairingId: null }); });
}

export function topLevelSpaceForThread(threads: Thread[], threadId: number): Thread | null {
  const index = threadIndex(threads);
  const thread = index.get(threadId);
  if (!thread) return null;
  const path = [...ancestorsIn(index, threadId), thread];
  return path.find(candidate => candidate.parent_thread_id === null && candidate.kind === "space") ?? null;
}

/** The nearest Space chat that contains this Thread. */
export function spaceForThread(threads: Thread[], threadId: number): Thread | null {
  const index = threadIndex(threads);
  const thread = index.get(threadId);
  if (!thread) return null;
  if (thread.kind === "space") return thread;
  return [...ancestorsIn(index, threadId)].reverse().find(candidate => candidate.kind === "space") ?? null;
}

export function enterSpace(spaceId: number): void {
  setSpaceState(draft => { Object.assign(draft, {
    spaceRecipientId: spaceId,
    workerPairingId: null,
  }); });
}

export function stepIntoWorker(threads: Thread[], threadId: number): void {
  const space = spaceForThread(threads, threadId);
  setSpaceState(draft => { Object.assign(draft, {
    spaceRecipientId: space?.id ?? null,
    workerPairingId: threadId,
  }); });
}
