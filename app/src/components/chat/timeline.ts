// Pure fold from the running turn's ordered `turn_event`s (v1.5) into rendered
// timeline items. Kept free of any Solid/DOM concerns so it can be unit tested
// directly (see timeline.test.tsx) and reused by both the live timeline under
// the thinking marker and the committed-turn "turn details" view.
import type { TimelineEvent } from "../../store/types";

/** Where a started step (a tool call, an Agent program cell) has got to. A step
 * is running until its `done` arrives; everything an outcome carries — the
 * ok/fail verdict, the result payload, the measured duration — exists only in
 * the done state, so "finished but with no verdict" is not representable. */
export type StepStatus =
  | { state: "running" }
  | {
      state: "done";
      ok: boolean;
      result: string | null;
      /** Wall time between the start and done arrivals; null when either
       * endpoint carried no client timestamp (hand-built or replayed events). */
      durationMs: number | null;
    };

/** A rendered timeline row. Prose/reasoning blocks carry accumulated markdown;
 * a tool row carries its start summary plus its `status`, which holds the done
 * result once it resolves. */
export type TimelineItem =
  | { kind: "prose"; key: string; text: string }
  | { kind: "reasoning"; key: string; text: string }
  | {
      kind: "tool";
      key: string;
      toolId: string;
      name: string;
      summary: string | null;
      status: StepStatus;
    }
  | {
      kind: "code";
      key: string;
      codeId: string;
      language: string;
      code: string;
      truncated: boolean;
      status: StepStatus;
    };

/** A row that a start/done pair drives. */
type StepItem = Extract<TimelineItem, { status: StepStatus }>;

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

/**
 * One start/done pairing, over its OWN id namespace.
 *
 * Tools and code cells stream independent id spaces, so each fold gets its own
 * pairing: an id can never resolve — or borrow a duration from — a row of the
 * other kind. `start` opens a row at the current position; `done` resolves the
 * matching open row in place, or, when its start never arrived (a reconnect
 * mid-turn dropped it), appends the already-completed row `orphan` builds.
 */
function stepPairing(items: TimelineItem[]) {
  const indexById = new Map<string, number>();
  const startedAt = new Map<string, number>();
  return {
    start(id: string, at: number | undefined, row: StepItem): void {
      indexById.set(id, items.length);
      if (at !== undefined) startedAt.set(id, at);
      items.push(row);
    },
    done(
      id: string,
      at: number | undefined,
      outcome: { ok: boolean; result: string | null },
      orphan: () => StepItem,
    ): void {
      const from = startedAt.get(id);
      const status: StepStatus = {
        state: "done",
        ok: outcome.ok,
        result: outcome.result,
        durationMs: at !== undefined && from !== undefined ? at - from : null,
      };
      const idx = indexById.get(id);
      const row = idx === undefined ? undefined : items[idx];
      if (row === undefined) {
        indexById.set(id, items.length);
        items.push({ ...orphan(), status });
        return;
      }
      if (!("status" in row)) return;
      row.status = status;
    },
  };
}

export function buildTimeline(events: TimelineEvent[], showCode = false): TimelineItem[] {
  const items: TimelineItem[] = [];
  const tools = stepPairing(items);
  const code = stepPairing(items);

  for (const { seq, event, at } of events) {
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
        tools.start(event.id, at, {
          kind: "tool",
          key: `tool-${event.id}`,
          toolId: event.id,
          name: event.name,
          summary: event.summary,
          status: { state: "running" },
        });
        break;
      }
      case "tool_done": {
        // An orphan done is labelled from its own `name` — the start carried
        // the summary, so there is none to show.
        tools.done(event.id, at, { ok: event.ok, result: event.summary }, () => ({
          kind: "tool",
          key: `tool-${event.id}`,
          toolId: event.id,
          name: event.name,
          summary: null,
          status: { state: "running" },
        }));
        break;
      }
      case "code_start": {
        if (!showCode) break;
        code.start(event.id, at, {
          kind: "code",
          key: `code-${event.id}`,
          codeId: event.id,
          language: event.language,
          code: event.code,
          truncated: event.truncated,
          status: { state: "running" },
        });
        break;
      }
      case "code_done": {
        if (!showCode) break;
        // An orphan done has no source to show, only the cell's outcome — which
        // still beats dropping it silently.
        code.done(event.id, at, { ok: event.ok, result: event.summary }, () => ({
          kind: "code",
          key: `code-${event.id}`,
          codeId: event.id,
          language: "",
          code: "",
          truncated: false,
          status: { state: "running" },
        }));
        break;
      }
    }
  }

  return items;
}
