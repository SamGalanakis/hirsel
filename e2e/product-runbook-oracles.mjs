import { fromMarkdown } from "../app/node_modules/mdast-util-from-markdown/index.js";
import { gfmFromMarkdown } from "../app/node_modules/mdast-util-gfm/index.js";
import { toString as mdastToString } from "../app/node_modules/mdast-util-to-string/index.js";
import { gfm } from "../app/node_modules/micromark-extension-gfm/index.js";
import remend from "../app/node_modules/remend/dist/index.js";

export function renderedInlineCodeText(markdown) {
  return markdown.replace(/`([^`\r\n]+)`/g, "$1");
}

export function renderedMarkdownText(markdown) {
  const source = remend(markdown, { linkMode: "text-only" });
  return mdastToString(fromMarkdown(source, { extensions: [gfm()], mdastExtensions: [gfmFromMarkdown()] }));
}

export function contiguousTextBlocks(events) {
  const blocks = [];
  let active = null;
  for (const [index, { event }] of events.entries()) {
    if ((event.kind === "prose" || event.kind === "reasoning") && event.text) {
      if (active?.kind === event.kind && active.lastIndex === index - 1) {
        active.text += event.text;
        active.lastIndex = index;
        active.eventCount += 1;
      } else {
        active = { kind: event.kind, text: event.text, firstIndex: index, lastIndex: index, eventCount: 1 };
        blocks.push(active);
      }
    } else {
      active = null;
    }
  }
  return blocks;
}

/** A program that only reports back carries nothing the Owner asked about, so
 * the trace drops it. Mirrors `TRIVIAL_FINISH` in
 * `app/src/components/chat/timeline.ts`; the two must agree or this oracle
 * stops describing the rendered trace. */
const TRIVIAL_FINISH = /^(?:await\s+)?finish\(\s*(?:"(?:[^"\\]|\\[\s\S])*"|'(?:[^'\\]|\\[\s\S])*'|`(?:[^`\\$]|\\[\s\S]|\$(?!\{))*`)?\s*\)\s*;?$/;

export function renderedTimelineExpectation(events) {
  let activityEnd = events.length;
  while (activityEnd > 0 && events[activityEnd - 1].event.kind === "prose") activityEnd -= 1;
  const rows = [];
  const toolRows = new Set();
  const codeRows = new Set();
  const skippedCode = new Set();
  for (const [index, { event }] of events.slice(0, activityEnd).entries()) {
    if (event.kind === "prose" || event.kind === "reasoning") {
      if (!event.text) continue;
      const previous = rows.at(-1);
      if (previous?.slot === `timeline-${event.kind}` && previous.lastIndex === index - 1) {
        previous.rawText += event.text;
        previous.text = renderedMarkdownText(previous.rawText);
        previous.lastIndex = index;
      } else {
        rows.push({ slot: `timeline-${event.kind}`, toolCallId: null, codeId: null, rawText: event.text, text: renderedMarkdownText(event.text), lastIndex: index });
      }
    } else if (event.kind === "tool_start" || (event.kind === "tool_done" && !toolRows.has(event.id))) {
      rows.push({ slot: "timeline-tool", toolCallId: event.id, codeId: null });
      toolRows.add(event.id);
    } else if (event.kind === "code_start") {
      // The Agent's own program cell is a peer of the tool rows beside it, in
      // arrival order — the Owner reads one flat sequence, not a tree.
      if (TRIVIAL_FINISH.test(event.code.trim()) && !event.truncated) { skippedCode.add(event.id); continue; }
      rows.push({ slot: "timeline-code", toolCallId: null, codeId: event.id });
      codeRows.add(event.id);
    } else if (event.kind === "code_done" && !codeRows.has(event.id) && !skippedCode.has(event.id)) {
      rows.push({ slot: "timeline-code", toolCallId: null, codeId: event.id });
      codeRows.add(event.id);
    }
  }
  const rawReply = events.slice(activityEnd).map(({ event }) => event.kind === "prose" ? event.text : "").join("");
  return { rows, rawReply, reply: rawReply ? renderedMarkdownText(rawReply) : "" };
}

export function hasExactAdjacentDuplicate(text) {
  return text.length > 0 && text.length % 2 === 0 && text.slice(0, text.length / 2) === text.slice(text.length / 2);
}
