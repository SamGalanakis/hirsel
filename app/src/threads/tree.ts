import { instant } from "./conversation";
import { threadSection, type ThreadSection } from "./model";
import type { Thread } from "./types";

/** Identity paths for humans. Agent reference resolution and access live in the host. */
export function threadAncestors(threads: Thread[], id: number): Thread[] {
  const byId = new Map(threads.map(thread => [thread.id, thread]));
  const seen = new Set([id]);
  const parents: Thread[] = [];
  let parent = byId.get(id)?.parent_thread_id;
  while (parent != null && !seen.has(parent)) {
    seen.add(parent);
    const thread = byId.get(parent);
    if (!thread) break;
    parents.unshift(thread);
    parent = thread.parent_thread_id;
  }
  return parents;
}
export function threadPath(threads: Thread[], id: number): string {
  return [...threadAncestors(threads, id), ...threads.filter(thread => thread.id === id)]
    .map(thread => `${thread.title} #${thread.id}`).join(" / ");
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
export function threadTree(threads: Thread[], section: ThreadSection, now: number, expanded: ReadonlySet<number>): ThreadTreeRow[] {
  const included = new Set<number>();
  const matches = new Set(threads.filter(thread => threadSection(thread, now) === section).map(thread => thread.id));
  const contextParents = new Set<number>();
  for (const id of matches) {
    included.add(id);
    for (const parent of threadAncestors(threads, id)) {
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
      const open = expanded.has(thread.id) || contextParents.has(thread.id);
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
