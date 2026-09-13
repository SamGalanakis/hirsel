import { instant } from "./conversation";
import { threadSection, type ThreadSection } from "./model";
import type { Thread } from "./types";

/** A snapshot's threads keyed by id. Ancestry is a walk over this index, so
 * anything that resolves more than one thread builds it once and reuses it
 * instead of rebuilding a Map per lookup. */
export type ThreadIndex = ReadonlyMap<number, Thread>;
export function threadIndex(threads: Thread[]): ThreadIndex {
  return new Map(threads.map(thread => [thread.id, thread]));
}
export function ancestorsIn(index: ThreadIndex, id: number): Thread[] {
  const seen = new Set([id]);
  const parents: Thread[] = [];
  let parent = index.get(id)?.parent_thread_id;
  while (parent != null && !seen.has(parent)) {
    seen.add(parent);
    const thread = index.get(parent);
    if (!thread) break;
    parents.unshift(thread);
    parent = thread.parent_thread_id;
  }
  return parents;
}
export function pathIn(index: ThreadIndex, id: number): string {
  const self = index.get(id);
  return [...ancestorsIn(index, id), ...(self ? [self] : [])]
    .map(thread => `${thread.title} #${thread.id}`).join(" / ");
}
/** Identity paths for humans. Agent reference resolution and access live in the
 * host. These one-shot forms are for a single lookup (one row, one tooltip);
 * callers resolving many threads from the same snapshot take an index instead. */
export function threadAncestors(threads: Thread[], id: number): Thread[] {
  return ancestorsIn(threadIndex(threads), id);
}
export function threadPath(threads: Thread[], id: number): string {
  return pathIn(threadIndex(threads), id);
}
function rootOrder(a: Thread, b: Thread): number {
  const leftPin = a.parent_thread_id === null ? a.pinned_at : null;
  const rightPin = b.parent_thread_id === null ? b.pinned_at : null;
  if (leftPin && rightPin) {
    const left = instant(leftPin), right = instant(rightPin);
    return left < right ? -1 : left > right ? 1 : a.id - b.id;
  }
  return leftPin ? -1 : rightPin ? 1 : a.id - b.id;
}
export interface ThreadTreeRow { thread: Thread; depth: number; context: boolean; hasChildren: boolean; expanded: boolean }
/** Filter matches keep their ancestry. Pinned roots sort first in the same forest;
 * every Thread appears once. Defensive traversal keeps malformed snapshots bounded. */
export type ThreadExpansion = ReadonlySet<number> | ((thread: Thread, depth: number) => boolean);
export function threadTree(threads: Thread[], section: ThreadSection, now: number, expanded: ThreadExpansion): ThreadTreeRow[] {
  const isExpanded = typeof expanded === "function" ? expanded : (thread: Thread) => expanded.has(thread.id);
  const index = threadIndex(threads);
  const included = new Set<number>();
  const matches = new Set(threads.filter(thread => threadSection(thread, now) === section).map(thread => thread.id));
  const contextParents = new Set<number>();
  for (const id of matches) {
    included.add(id);
    for (const parent of ancestorsIn(index, id)) {
      included.add(parent.id);
      if (!matches.has(parent.id)) contextParents.add(parent.id);
    }
  }
  const children = new Map<number | null, Thread[]>();
  const candidates = threads.filter(thread => included.has(thread.id)).sort((a, b) => a.id - b.id);
  for (const thread of candidates) {
    const parent = thread.parent_thread_id !== null && included.has(thread.parent_thread_id) ? thread.parent_thread_id : null;
    children.set(parent, [...(children.get(parent) ?? []), thread]);
  }
  const rows: ThreadTreeRow[] = [];
  const seen = new Set<number>();
  const visit = (root: Thread) => {
    const stack = [{ thread: root, depth: 0 }];
    while (stack.length) {
      const { thread, depth } = stack.pop()!;
      if (seen.has(thread.id)) continue;
      seen.add(thread.id);
      const nested = children.get(thread.id) ?? [];
      const open = nested.length > 0 && (isExpanded(thread, depth) || contextParents.has(thread.id));
      rows.push({ thread, depth, context: !matches.has(thread.id), hasChildren: nested.length > 0, expanded: open });
      if (open) stack.push(...nested.toReversed().map(child => ({ thread: child, depth: depth + 1 })));
      else {
        const hidden = [...nested];
        while (hidden.length) { const child = hidden.pop()!; if (seen.has(child.id)) continue; seen.add(child.id); hidden.push(...(children.get(child.id) ?? [])); }
      }
    }
  };
  for (const root of (children.get(null) ?? []).sort(rootOrder)) visit(root);
  // Orphans/cycles remain reachable to humans rather than being silently discarded.
  for (const thread of candidates) if (!seen.has(thread.id)) visit(thread);
  return rows;
}
