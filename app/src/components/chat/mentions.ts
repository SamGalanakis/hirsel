// @-mention plumbing for the Composer (v2.1, ADR-0009 addendum). Pure, so the
// detection/resolution/insertion logic is unit-tested directly and shared by
// both the desktop popup and the phone quick-select sheet.
//
// The robust source of truth for `send_message.mentions` is the composed TEXT:
// on send we re-parse `@handle` tokens out of the body and resolve them against
// the currently-open Pings. That keeps text ↔ mentions in sync for free —
// delete the token and the mention drops; there is no separate list to reconcile.
import type { Ping } from "../../protocol";

/** Characters allowed inside an @handle token. Ping names are short slugs
 * (`delegated-fix-ready`, `release.choice`), so letters/digits plus `-`, `_`,
 * `.` cover them; a space or any other char ends the token. */
const NAME_CHAR = /[A-Za-z0-9._-]/;

/** A `@handle` mention is only triggered at a word boundary — start of input or
 * after whitespace / an opening paren — so an email or `a@b` never opens it. */
function isTriggerBoundary(ch: string | undefined): boolean {
  return ch === undefined || /[\s(]/.test(ch);
}

export interface MentionQuery {
  /** Index of the `@` in the text. */
  start: number;
  /** The handle text typed after `@`, up to the caret (may be ""). */
  query: string;
}

/** If the caret sits inside a `@handle` token being typed, return that token's
 * `@` position and the query so far; otherwise null. An empty query (caret
 * right after a lone `@`) still returns — that's what opens the picker showing
 * every open Ping. */
export function detectMentionQuery(text: string, caret: number): MentionQuery | null {
  let i = caret;
  while (i > 0 && NAME_CHAR.test(text[i - 1])) i--;
  // The char just before the run must be the trigger `@`.
  if (i === 0 || text[i - 1] !== "@") return null;
  const at = i - 1;
  if (!isTriggerBoundary(text[at - 1])) return null;
  return { start: at, query: text.slice(i, caret) };
}

/** Open Pings whose `name` matches the query (case-insensitive). An empty query
 * lists all open Pings. `startsWith` matches sort ahead of mere substring
 * matches; ties keep newest-first (highest id). Capped by `limit`. */
export function filterMentionCandidates(
  pings: Ping[],
  query: string,
  limit = 6,
): Ping[] {
  const q = query.toLowerCase();
  const open = pings.filter((p) => p.status === "open");
  const matches = q.length === 0 ? open : open.filter((p) => p.name.toLowerCase().includes(q));
  return matches
    .sort((a, b) => {
      const aStarts = a.name.toLowerCase().startsWith(q) ? 0 : 1;
      const bStarts = b.name.toLowerCase().startsWith(q) ? 0 : 1;
      if (aStarts !== bStarts) return aStarts - bStarts;
      return b.id - a.id;
    })
    .slice(0, limit);
}

/** Replace the in-progress `@query` (from its `@` up to `caret`) with the chosen
 * Ping's `@name` plus a trailing space, returning the new text and caret. */
export function insertMention(
  text: string,
  query: MentionQuery,
  caret: number,
  name: string,
): { text: string; caret: number } {
  const before = text.slice(0, query.start);
  const after = text.slice(caret);
  const token = `@${name} `;
  const nextText = `${before}${token}${after}`;
  return { text: nextText, caret: before.length + token.length };
}

/** Resolve every `@handle` token in the composed text to an open Ping id (the
 * outgoing `send_message.mentions`). Deduped, order-preserving. Mentions are
 * lifecycle-neutral: this never resolves a Ping, only names it for the turn. */
export function resolveMentionIds(text: string, pings: Ping[]): number[] {
  const openByName = new Map<string, number>();
  for (const p of pings) {
    if (p.status === "open") openByName.set(p.name.toLowerCase(), p.id);
  }
  const ids: number[] = [];
  const seen = new Set<number>();
  const re = /(^|[\s(])@([A-Za-z0-9._-]+)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    const id = openByName.get(m[2].toLowerCase());
    if (id !== undefined && !seen.has(id)) {
      seen.add(id);
      ids.push(id);
    }
  }
  return ids;
}
