// Task refs — hirsel's one citation form.
//
// A Task's durable identity on the wire is its numeric id, so the ref IS that
// id with a `#` in front: `#12`. One string, one prefix, and the SAME string
// everywhere a Task is cited — on its chip, on its focused card, in a draft, in
// a sent message, and as the tail of its `/t/12` deep link. Nothing has to be
// reconciled because there is only ever one spelling to reconcile.
//
// `#` rather than `@`: `@` means a person in every convention that uses it
// (Slack, Notion, GitHub), and hirsel has exactly one person in it. `#` is what
// every tool that cites *work* by id already uses (GitHub issues, Linear,
// commit messages), and it composes with the terminal register the product
// speaks in.
//
// The composed TEXT is the source of truth for `send_message.mentions`: on send
// the body is re-parsed and each `#id` resolved against the live Task set. Delete
// the token and the mention drops with it; there is no parallel list to keep in
// step.

import type { EventItem } from "../protocol";

/** The character that opens the picker. */
export const TASK_REF_TRIGGER = "#";

/** Characters allowed inside a ref query. The ref itself is digits, but the
 * picker also filters by Task name, so a partially-typed name (`#depl`) is a
 * live query too. A space or any other character ends the token. */
const QUERY_CHAR = /[A-Za-z0-9._-]/;

/** A ref is only a ref at a word boundary — start of input, or after whitespace
 * or an opening bracket. That keeps `abc#4` and a `#rrggbb` colour out of it. */
function isTriggerBoundary(ch: string | undefined): boolean {
  return ch === undefined || /[\s([]/.test(ch);
}

/** The one spelling of a Task ref. */
export function formatTaskRef(id: number): string {
  return `${TASK_REF_TRIGGER}${id}`;
}

/** Parse a bare ref (`#12`, or a lone `12`) to its Task id, or null. */
export function parseTaskRef(token: string): number | null {
  const match = /^#?(\d+)$/.exec(token.trim());
  if (!match) return null;
  const id = Number.parseInt(match[1], 10);
  return Number.isSafeInteger(id) && id > 0 ? id : null;
}

export interface RefQuery {
  /** Index of the `#` in the text. */
  start: number;
  /** What has been typed after `#`, up to the caret (may be ""). */
  query: string;
}

/** If the caret sits inside a `#…` token being typed, return that token's `#`
 * position and the query so far; otherwise null. An empty query (caret right
 * after a lone `#`) still returns — that is what opens the picker on the whole
 * Task field. */
export function detectRefQuery(text: string, caret: number): RefQuery | null {
  let i = caret;
  while (i > 0 && QUERY_CHAR.test(text[i - 1])) i--;
  if (i === 0 || text[i - 1] !== TASK_REF_TRIGGER) return null;
  const at = i - 1;
  if (!isTriggerBoundary(text[at - 1])) return null;
  return { start: at, query: text.slice(i, caret) };
}

function normalize(name: string): string {
  return name.replace(/^@/, "").toLowerCase();
}

/** How well one Task answers a query — lower is a better answer. `null` means
 * it does not answer it at all. */
function rank(task: EventItem, query: string): number | null {
  if (query.length === 0) return 4;
  const id = String(task.id);
  if (id === query) return 0;
  if (id.startsWith(query)) return 1;
  const name = normalize(task.name);
  const q = query.toLowerCase().replaceAll(" ", "-");
  if (name.startsWith(q)) return 2;
  if (name.includes(q)) return 3;
  return null;
}

/** Tasks answering `query`, best answer first; ties go newest-first (higher id).
 * An empty query lists the whole field, capped at `limit`. */
export function filterTaskCandidates(
  tasks: EventItem[],
  query: string,
  limit = 6,
): EventItem[] {
  return tasks
    .map((task) => ({ task, rank: rank(task, query) }))
    .filter((row): row is { task: EventItem; rank: number } => row.rank !== null)
    .sort((a, b) => (a.rank !== b.rank ? a.rank - b.rank : b.task.id - a.task.id))
    .slice(0, limit)
    .map((row) => row.task);
}

/** Replace the in-progress `#query` (from its `#` up to `caret`) with the chosen
 * Task's ref plus a trailing space, returning the new text and caret. */
export function insertTaskRef(
  text: string,
  query: RefQuery,
  caret: number,
  id: number,
): { text: string; caret: number } {
  const before = text.slice(0, query.start);
  const after = text.slice(caret);
  // Never double the separator, and always leave the caret past it: inserting
  // mid-sentence must feel exactly like inserting at the end.
  const spaced = after.startsWith(" ");
  const token = spaced ? formatTaskRef(id) : `${formatTaskRef(id)} `;
  return {
    text: `${before}${token}${after}`,
    caret: before.length + token.length + (spaced ? 1 : 0),
  };
}

/** Every `#id` in the text, as [index, id] pairs in source order. Word-bounded
 * on both sides, so `#1234ab` and `abc#4` are ordinary text. */
function scanRefs(text: string): { index: number; id: number; token: string }[] {
  const out: { index: number; id: number; token: string }[] = [];
  const re = /(^|[\s([])#(\d+)(?![\w-])/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    const id = Number.parseInt(m[2], 10);
    if (Number.isSafeInteger(id)) {
      out.push({ index: m.index + m[1].length, id, token: `#${m[2]}` });
    }
  }
  return out;
}

/** Resolve every ref in a composed body to a live Task id — the outgoing
 * `send_message.mentions`. Deduped, order-preserving, and silent about refs that
 * name nothing in the field: an unresolvable `#99` stays plain text and is never
 * sent as a mention. */
export function resolveMentionIds(text: string, tasks: EventItem[]): number[] {
  const known = new Set(tasks.map((task) => task.id));
  const ids: number[] = [];
  const seen = new Set<number>();
  for (const ref of scanRefs(text)) {
    if (!known.has(ref.id) || seen.has(ref.id)) continue;
    seen.add(ref.id);
    ids.push(ref.id);
  }
  return ids;
}

/** One run of rendered text: either plain prose, or a resolved Task ref. */
export interface RefSpan {
  text: string;
  /** The cited Task, or null when this span is ordinary text. */
  taskId: number | null;
}

/** Split a text run into plain and ref spans for rendering. A ref naming no
 * live Task stays in a plain span, so an unknown or archived citation degrades
 * to exactly the characters the author typed. */
export function splitTaskRefs(text: string, isKnown: (id: number) => boolean): RefSpan[] {
  const spans: RefSpan[] = [];
  let cursor = 0;
  for (const ref of scanRefs(text)) {
    if (!isKnown(ref.id)) continue;
    if (ref.index > cursor) spans.push({ text: text.slice(cursor, ref.index), taskId: null });
    spans.push({ text: ref.token, taskId: ref.id });
    cursor = ref.index + ref.token.length;
  }
  if (cursor < text.length) spans.push({ text: text.slice(cursor), taskId: null });
  return spans;
}
