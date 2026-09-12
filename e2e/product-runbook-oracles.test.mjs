import assert from "node:assert/strict";
import test from "node:test";

import {
  contiguousTextBlocks,
  hasExactAdjacentDuplicate,
  renderedInlineCodeText,
  renderedMarkdownText,
  renderedTimelineExpectation,
} from "./product-runbook-oracles.mjs";

test("rendered prompt comparison ignores inline-code delimiters but preserves content", () => {
  const raw = "Run `node test-calculator.mjs`, then replace `left - right` with `left + right`.";
  const rendered = "Run node test-calculator.mjs, then replace left - right with left + right.";

  assert.equal(renderedInlineCodeText(raw), rendered);
  assert.notEqual(renderedInlineCodeText(raw), rendered.replace("test-calculator.mjs", "other.mjs"));
});

test("rendered reply comparison assembles markdown split across stream chunks", () => {
  const events = [
    { seq: 20, event: { kind: "prose", text: "Changed files:\n- `calculator.mjs`: `add" } },
    { seq: 21, event: { kind: "prose", text: "` returns `left + right`.\n- **Summary** retained." } },
  ];

  assert.deepEqual(contiguousTextBlocks(events), [{
    kind: "prose",
    text: "Changed files:\n- `calculator.mjs`: `add` returns `left + right`.\n- **Summary** retained.",
    firstIndex: 0,
    lastIndex: 1,
    eventCount: 2,
  }]);
  assert.equal(renderedMarkdownText(contiguousTextBlocks(events)[0].text), "Changed files:calculator.mjs: add returns left + right.Summary retained.");
  assert.equal(renderedMarkdownText("WORKER_FIXED_native-run"), "WORKER_FIXED_native-run");
  assert.deepEqual(renderedTimelineExpectation(events), {
    rows: [],
    rawReply: contiguousTextBlocks(events)[0].text,
    reply: "Changed files:calculator.mjs: add returns left + right.Summary retained.",
  });
  assert.notEqual(renderedTimelineExpectation(events).reply, "Changed files:calculator.mjs: add returns left + right.");
});

test("rendered timeline expectation preserves interleaved row order", () => {
  const events = [
    { seq: 0, event: { kind: "prose", text: "Inspect " } },
    { seq: 1, event: { kind: "prose", text: "first." } },
    { seq: 2, event: { kind: "tool_start", id: "read-1", name: "read" } },
    { seq: 3, event: { kind: "tool_done", id: "read-1", name: "read" } },
    { seq: 4, event: { kind: "reasoning", text: "Then " } },
    { seq: 5, event: { kind: "reasoning", text: "edit." } },
    { seq: 6, event: { kind: "prose", text: "Final `reply" } },
    { seq: 7, event: { kind: "prose", text: "` intact." } },
  ];
  const expected = renderedTimelineExpectation(events);

  assert.deepEqual(expected.rows.map(({ slot, toolCallId, text }) => ({ slot, toolCallId, text })), [
    { slot: "timeline-prose", toolCallId: null, text: "Inspect first." },
    { slot: "timeline-tool", toolCallId: "read-1", text: undefined },
    { slot: "timeline-reasoning", toolCallId: null, text: "Then edit." },
  ]);
  assert.equal(expected.rawReply, "Final `reply` intact.");
  assert.equal(expected.reply, "Final reply intact.");
});

test("contiguous reasoning blocks retain exact repeated content for integrity checks", () => {
  const phrase = "Run the focused test before editing.";
  const events = [
    { seq: 5, event: { kind: "reasoning", text: phrase.slice(0, 12) } },
    { seq: 6, event: { kind: "reasoning", text: `${phrase.slice(12)}${phrase}` } },
    { seq: 7, event: { kind: "tool_start", id: "call-1", name: "read" } },
    { seq: 8, event: { kind: "reasoning", text: "A distinct thought." } },
  ];
  const reasoning = contiguousTextBlocks(events).filter(block => block.kind === "reasoning");

  assert.equal(reasoning[0].text, phrase + phrase);
  assert.equal(hasExactAdjacentDuplicate(reasoning[0].text), true);
  assert.equal(hasExactAdjacentDuplicate(reasoning[1].text), false);
});
