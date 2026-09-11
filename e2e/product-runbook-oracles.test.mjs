import assert from "node:assert/strict";
import test from "node:test";

import { renderedInlineCodeText } from "./product-runbook-oracles.mjs";

test("rendered prompt comparison ignores inline-code delimiters but preserves content", () => {
  const raw = "Run `node test-calculator.mjs`, then replace `left - right` with `left + right`.";
  const rendered = "Run node test-calculator.mjs, then replace left - right with left + right.";

  assert.equal(renderedInlineCodeText(raw), rendered);
  assert.notEqual(renderedInlineCodeText(raw), rendered.replace("test-calculator.mjs", "other.mjs"));
});
