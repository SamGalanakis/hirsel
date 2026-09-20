// Pure fold from the running turn's ordered `turn_event`s (v1.5) into rendered
// timeline items. Kept free of any Solid/DOM concerns so it can be unit tested
// directly (see timeline.test.tsx) and reused by both live and committed inline
// turn activity.
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
      summary: string | null;
      result: string | null;
      resultTruncated: boolean;
      /** Wall time between the start and done arrivals; null when either
       * endpoint carried no client timestamp (hand-built or replayed events). */
      durationMs: number | null;
    };

/** A rendered timeline row. Prose/reasoning blocks carry accumulated markdown;
 * a tool row carries its start summary plus its `status`, which holds the done
 * result once it resolves. */
export type TimelineItem =
  | { kind: "prose"; key: string; text: string; blockId?: string }
  | { kind: "reasoning"; key: string; text: string; blockId?: string }
  | {
      kind: "tool";
      key: string;
      toolId: string;
      name: string;
      summary: string | null;
      input: string | null;
      inputTruncated: boolean;
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

/** A tool row. */
export type ToolItem = Extract<TimelineItem, { kind: "tool" }>;

/** A step the Owner can open: a tool call or an Agent program cell. Both read
 * as pills on one flat row, so they share a shape. */
export type StepRowItem = Extract<TimelineItem, { kind: "tool" | "code" }>;
export function isStepRow(item: TimelineItem): item is StepRowItem {
  return item.kind === "tool" || item.kind === "code";
}

/** Every tool row in the timeline, in order. */
export function timelineTools(items: TimelineItem[]): ToolItem[] {
  return items.filter((item): item is ToolItem => item.kind === "tool");
}

/** A row that a start/done pair drives. */
type StepItem = Extract<TimelineItem, { status: StepStatus }>;

/**
 * Fold ordered timeline events into interleaved items, exactly in `seq` order:
 *
 * - Consecutive same-kind prose/reasoning deltas with the same optional block
 *   identity accumulate into one block/run. A changed identity or kind closes
 *   it. Legacy events have no identity and retain the old contiguous behavior.
 * - `tool_start` inserts a tool row at its position; `tool_done` resolves the
 *   matching-`id` row in place (spinner → ok/fail + result). A `tool_done` with
 *   no matching open row (e.g. a reconnect mid-turn dropped the start) is not
 *   discarded — it inserts an already-completed row labelled from its own `name`.
 *
 * - `code_start`/`code_done` behave exactly like the tool pair, but carry the
 *   Agent's verbatim program for the cell. A cell and the tools it called are
 *   peers in arrival order — the sequence is what the Owner reads, not a tree.
 *   A cell whose whole program is a trivial `finish()` is dropped: it is the
 *   wake protocol, not work the Owner asked about.
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

function joinTextRun(events: TimelineEvent[], kind: "prose" | "reasoning"): string {
  let text = "";
  let blockId: string | undefined;
  let hasBlock = false;
  for (const { event } of events) {
    if (event.kind !== kind) continue;
    if (hasBlock && blockId !== event.block_id) text += "\n\n";
    text += event.text;
    blockId = event.block_id;
    hasBlock = true;
  }
  return text;
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
    reply: joinTextRun(trailing, "prose"),
  };
}

/**
 * Is the turn's last act a reasoning delta — i.e. is `Timeline`'s trailing item
 * a reasoning run still being written?
 *
 * The same rule `Timeline` applies to its folded items, stated at the event
 * level so the thinking marker can ask it without folding twice. The two agree
 * by construction: only a prose or reasoning delta can APPEND a trailing block,
 * and `splitStreamingReply` has already taken the trailing prose away, so a
 * reasoning event at the tail is exactly a reasoning item at the tail.
 */
export function isReasoningTail(events: TimelineEvent[]): boolean {
  return events[events.length - 1]?.event.kind === "reasoning";
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
function stepPairing() {
  const placeById = new Map<string, { list: TimelineItem[]; index: number }>();
  const startedAt = new Map<string, number>();
  return {
    start(list: TimelineItem[], id: string, at: number | undefined, row: StepItem): void {
      placeById.set(id, { list, index: list.length });
      if (at !== undefined) startedAt.set(id, at);
      list.push(row);
    },
    done(
      list: TimelineItem[],
      id: string,
      at: number | undefined,
      outcome: { ok: boolean; summary: string | null; result: string | null; resultTruncated: boolean },
      orphan: () => StepItem,
    ): void {
      const from = startedAt.get(id);
      const status: StepStatus = {
        state: "done",
        ok: outcome.ok,
        summary: outcome.summary,
        result: outcome.result,
        resultTruncated: outcome.resultTruncated,
        durationMs: at !== undefined && from !== undefined ? at - from : null,
      };
      const place = placeById.get(id);
      const row = place === undefined ? undefined : place.list[place.index];
      if (row === undefined) {
        placeById.set(id, { list, index: list.length });
        list.push({ ...orphan(), status });
        return;
      }
      if (!("status" in row)) return;
      row.status = status;
    },
  };
}

/** A program that only reports back: `finish(<string literal>)`, with or without
 * an argument, an `await`, or a trailing semicolon, and nothing else in it.
 * There is nothing to read — an empty finish is the wake protocol, and a
 * literal one says exactly what the prose right below the entry already says.
 * Another statement, a tool call, or a computed argument makes it real work. */
const TRIVIAL_FINISH = /^(?:await\s+)?finish\(\s*(?:"(?:[^"\\]|\\[\s\S])*"|'(?:[^'\\]|\\[\s\S])*'|`(?:[^`\\$]|\\[\s\S]|\$(?!\{))*`)?\s*\)\s*;?$/;
function trivialProgram(code: string): boolean {
  return TRIVIAL_FINISH.test(code.trim());
}

export function buildTimeline(events: TimelineEvent[]): TimelineItem[] {
  const items: TimelineItem[] = [];
  const tools = stepPairing();
  const code = stepPairing();
  const skipped = new Set<string>();

  for (const { seq, event, at } of events) {
    switch (event.kind) {
      case "prose":
      case "reasoning": {
        const last = items[items.length - 1];
        if (last && last.kind === event.kind && last.blockId === event.block_id) {
          last.text += event.text;
        } else {
          items.push({ kind: event.kind, key: `${event.kind}-${seq}`, text: event.text, blockId: event.block_id });
        }
        break;
      }
      case "tool_start": {
        tools.start(items, event.id, at, {
          kind: "tool",
          key: `tool-${event.id}`,
          toolId: event.id,
          name: event.name,
          summary: event.summary,
          input: event.input?.text ?? null,
          inputTruncated: event.input?.truncated ?? false,
          status: { state: "running" },
        });
        break;
      }
      case "tool_done": {
        // An orphan done is labelled from its own `name` — the start carried
        // the summary, so there is none to show.
        tools.done(items, event.id, at, {
          ok: event.ok,
          summary: event.summary,
          result: event.result?.text ?? null,
          resultTruncated: event.result?.truncated ?? false,
        }, () => ({
          kind: "tool",
          key: `tool-${event.id}`,
          toolId: event.id,
          name: event.name,
          summary: null,
          input: null,
          inputTruncated: false,
          status: { state: "running" },
        }));
        break;
      }
      case "code_start": {
        if (trivialProgram(event.code) && !event.truncated) { skipped.add(event.id); break; }
        const row: Extract<TimelineItem, { kind: "code" }> = {
          kind: "code",
          key: `code-${event.id}`,
          codeId: event.id,
          language: event.language,
          code: event.code,
          truncated: event.truncated,
          status: { state: "running" },
        };
        code.start(items, event.id, at, row);
        break;
      }
      case "code_done": {
        if (skipped.has(event.id)) break;
        // An orphan done has no source to show, only the cell's outcome — which
        // still beats dropping it silently.
        code.done(items, event.id, at, { ok: event.ok, summary: event.summary, result: event.summary, resultTruncated: false }, () => ({
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
