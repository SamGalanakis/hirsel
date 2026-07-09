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
 * Input is assumed already sorted by `seq` (the reducer keeps it so); this fold
 * never reorders.
 */
export function buildTimeline(events: TimelineEvent[]): TimelineItem[] {
  const items: TimelineItem[] = [];
  const toolIndexById = new Map<string, number>();

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
    }
  }

  return items;
}
