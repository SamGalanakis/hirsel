// Pure fold from the running turn's ordered `turn_event`s (v1.5) into rendered
// timeline items. Kept free of any Solid/DOM concerns so it can be unit tested
// directly (see timeline.test.tsx) and reused by both the live timeline under
// the thinking marker and the committed-turn "turn details" view.
import type { TimelineEvent } from "../../store/types";

/** A rendered timeline row. Prose/reasoning blocks carry accumulated markdown;
 * a tool row carries its start summary plus (once resolved) its done result. */
export type TimelineItem =
  | { kind: "prose"; key: string; text: string }
  | { kind: "reasoning"; key: string; text: string }
  | {
      kind: "tool";
      key: string;
      toolId: string;
      name: string;
      summary: string | null;
      done: boolean;
      ok: boolean | null;
      result: string | null;
    }
  | {
      kind: "code";
      key: string;
      codeId: string;
      language: string;
      code: string;
      truncated: boolean;
      done: boolean;
      ok: boolean | null;
      result: string | null;
    };

/**
 * Fold ordered timeline events into interleaved items, exactly in `seq` order:
 *
 * - Consecutive same-kind prose/reasoning deltas accumulate into one block/run;
 *   any change of kind (including a tool_start) closes the current block, and a
 *   later prose delta opens a new one.
 * - `tool_start` inserts a tool row at its position; `tool_done` resolves the
 *   matching-`id` row in place (spinner → ok/fail + result). A `tool_done` with
 *   no matching open row (e.g. a reconnect mid-turn dropped the start) is not
 *   discarded — it inserts an already-completed row labelled from its own `name`.
 *
 * - `code_start`/`code_done` behave exactly like the tool pair, but carry the
 *   Agent's verbatim program for the cell. They are only folded in when
 *   `showCode` is set (Settings → "Show agent code"); otherwise the events are
 *   dropped here, so nothing downstream has to know about the preference.
 *
 * Input is assumed already sorted by `seq` (the reducer keeps it so); this fold
 * never reorders.
 */
/** The running turn split into the work it is doing and the reply it is
 * currently writing. */
export interface StreamingSplit {
  /** Everything before the reply being written: tool rows, reasoning, and any
   * earlier prose the Agent has already moved on from. Rendered as the quiet
   * timeline. */
  activity: TimelineEvent[];
  /** The accumulated text of the trailing prose run — the sentence being
   * written right now. Empty when the turn's last act was a tool call or
   * reasoning, i.e. when no reply is in flight. */
  reply: string;
}

/**
 * Split a running turn's events into activity and the in-flight reply.
 *
 * The trailing run of consecutive `prose` deltas is the reply the Agent is
 * writing at this instant; rendering it in committed-message typography is what
 * makes a turn read as a chat reply arriving rather than a log scrolling. Any
 * prose block the Agent has already closed (by calling a tool or thinking)
 * stays in the timeline, where its provisional styling is honest.
 *
 * Exactly-once on commit falls out of this: the reply is derived from
 * `turnEvents`, which the reducer clears on the committing `msg`, and the
 * committed row renders in the same typography — so the draft is replaced in
 * place with no duplicate and no flash.
 */
export function splitStreamingReply(events: TimelineEvent[]): StreamingSplit {
  let start = events.length;
  while (start > 0 && events[start - 1].event.kind === "prose") start -= 1;
  const trailing = events.slice(start);
  return {
    activity: events.slice(0, start),
    reply: trailing.map(({ event }) => (event.kind === "prose" ? event.text : "")).join(""),
  };
}

export function buildTimeline(events: TimelineEvent[], showCode = false): TimelineItem[] {
  const items: TimelineItem[] = [];
  const toolIndexById = new Map<string, number>();
  const codeIndexById = new Map<string, number>();

  for (const { seq, event } of events) {
    switch (event.kind) {
      case "prose":
      case "reasoning": {
        const last = items[items.length - 1];
        if (last && last.kind === event.kind) {
          last.text += event.text;
        } else {
          items.push({ kind: event.kind, key: `${event.kind}-${seq}`, text: event.text });
        }
        break;
      }
      case "tool_start": {
        toolIndexById.set(event.id, items.length);
        items.push({
          kind: "tool",
          key: `tool-${event.id}`,
          toolId: event.id,
          name: event.name,
          summary: event.summary,
          done: false,
          ok: null,
          result: null,
        });
        break;
      }
      case "tool_done": {
        const idx = toolIndexById.get(event.id);
        if (idx === undefined) {
          // Orphan done (no matching tool_start — e.g. the start was lost across
          // a reconnect). Render it as an already-completed row from its own name.
          toolIndexById.set(event.id, items.length);
          items.push({
            kind: "tool",
            key: `tool-${event.id}`,
            toolId: event.id,
            name: event.name,
            summary: null,
            done: true,
            ok: event.ok,
            result: event.summary,
          });
          break;
        }
        const row = items[idx];
        if (row.kind !== "tool") break;
        row.done = true;
        row.ok = event.ok;
        row.result = event.summary;
        break;
      }
      case "code_start": {
        if (!showCode) break;
        codeIndexById.set(event.id, items.length);
        items.push({
          kind: "code",
          key: `code-${event.id}`,
          codeId: event.id,
          language: event.language,
          code: event.code,
          truncated: event.truncated,
          done: false,
          ok: null,
          result: null,
        });
        break;
      }
      case "code_done": {
        if (!showCode) break;
        const idx = codeIndexById.get(event.id);
        if (idx === undefined) {
          // Orphan done (the start was lost across a reconnect): still show the
          // cell's outcome rather than silently dropping it.
          codeIndexById.set(event.id, items.length);
          items.push({
            kind: "code",
            key: `code-${event.id}`,
            codeId: event.id,
            language: "",
            code: "",
            truncated: false,
            done: true,
            ok: event.ok,
            result: event.summary,
          });
          break;
        }
        const cell = items[idx];
        if (cell.kind !== "code") break;
        cell.done = true;
        cell.ok = event.ok;
        cell.result = event.summary;
        break;
      }
    }
  }

  return items;
}
