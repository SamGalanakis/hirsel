import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import test from "node:test";
import { createHighlighter, highlightRanges, splitLines, tokenTypes } from "../src/highlights";

const grammar = join(__dirname, "../grammar");
const canonical = join(__dirname, "../../../tree-sitter-hirsel/queries/highlights.scm");

test("the compiled grammar and staged query produce UTF-16 ranges", async () => {
  assert.deepEqual(await readFile(join(grammar, "highlights.scm")), await readFile(canonical));
  const { parser, query } = await createHighlighter(grammar);
  const source = 'fn f() { "é😀"; 42 }';
  const tree = parser.parse(source);
  assert.ok(tree);
  try {
    assert.equal(tree.rootNode.hasError, false);
    const ranges = highlightRanges(query, tree);
    const number = ranges.find((range) => tokenTypes[range.type] === "number");
    assert.ok(number);
    assert.deepEqual([number.start, number.end], [16, 18]);
    const string = ranges.find((range) => tokenTypes[range.type] === "string");
    assert.ok(string);
    assert.deepEqual([string.start, string.end], [9, 14]);
  } finally {
    tree.delete();
    query.delete();
    parser.delete();
  }
});

test("an attribute token splits at CRLF and skips empty lines", async () => {
  const { parser, query } = await createHighlighter(grammar);
  const source = "@compact(\r\n\r\nu32) type T {}";
  const tree = parser.parse(source);
  assert.ok(tree);
  try {
    assert.equal(tree.rootNode.hasError, false);
    const range = highlightRanges(query, tree).find((range) => tokenTypes[range.type] === "decorator");
    assert.ok(range);
    assert.deepEqual(Array.from(splitLines(source, range), (part) => [part.start, part.end]),
      [[0, 9], [13, 17]]);
  } finally {
    tree.delete();
    query.delete();
    parser.delete();
  }
});

test("method calls outrank fields and built-in types outrank generic type captures", async () => {
  const { parser, query } = await createHighlighter(grammar);
  const source = "fn f(x: u8) { x.m(); x.p }";
  const tree = parser.parse(source);
  assert.ok(tree);
  try {
    assert.equal(tree.rootNode.hasError, false);
    const raw = query.captures(tree.rootNode);
    assert.deepEqual(new Set(raw.filter((capture) => capture.node.text === "m")
      .map((capture) => capture.name)), new Set(["function.call", "property"]));
    assert.deepEqual(new Set(raw.filter((capture) => capture.node.text === "u8")
      .map((capture) => capture.name)), new Set(["type.builtin", "type"]));

    const ranges = highlightRanges(query, tree);
    assert.ok(ranges.every((range, index) => index === 0 || ranges[index - 1].end <= range.start));
    const tokens = ranges.map(({ start, end, type, modifiers }) =>
      [source.slice(start, end), tokenTypes[type], modifiers]);
    assert.deepEqual(tokens.filter(([text]) => ["m", "p", "u8"].includes(String(text))), [
      ["u8", "type", 1], ["m", "function", 0], ["p", "property", 0],
    ]);
  } finally {
    tree.delete();
    query.delete();
    parser.delete();
  }
});

test("custom semantic tokens have TextMate fallbacks", async () => {
  const manifest = JSON.parse(await readFile(join(__dirname, "../../package.json"), "utf8"));
  const [{ scopes }]: [{ scopes: Record<string, string[]> }] = manifest.contributes.semanticTokenScopes;
  assert.deepEqual(scopes, {
    decorator: ["storage.type.annotation"],
    hirselCharacter: ["constant.character"],
    hirselBracket: ["punctuation.bracket"],
    hirselDelimiter: ["punctuation.separator"],
    label: ["entity.name.label"],
  });
  assert.deepEqual(manifest.contributes.semanticTokenTypes.map(({ id }: { id: string }) => id),
    ["hirselCharacter", "hirselBracket", "hirselDelimiter"]);
});
