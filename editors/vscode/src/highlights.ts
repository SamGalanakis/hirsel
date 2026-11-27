import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { Language, Parser, Query } from "web-tree-sitter";

const captures = {
  keyword: "keyword",
  "keyword.conditional": "keyword",
  "keyword.repeat": "keyword",
  "keyword.return": "keyword",
  attribute: "decorator",
  "type.builtin": "type",
  type: "type",
  "type.definition": "type",
  constructor: "enumMember",
  function: "function",
  "function.call": "function",
  "variable.parameter": "parameter",
  property: "property",
  comment: "comment",
  string: "string",
  character: "hirselCharacter",
  number: "number",
  "number.float": "number",
  boolean: "keyword",
  label: "label",
  operator: "operator",
  "punctuation.bracket": "hirselBracket",
  "punctuation.delimiter": "hirselDelimiter",
} as const;

type CaptureName = keyof typeof captures;
export const tokenTypes = [...new Set(Object.values(captures))];
export const tokenModifiers = ["defaultLibrary", "declaration"];

type Highlight = {
  start: number;
  end: number;
  type: number;
  modifiers: number;
};

export async function createHighlighter(directory: string) {
  await Parser.init();
  const [language, source] = await Promise.all([
    Language.load(join(directory, "tree-sitter-hirsel.wasm")),
    readFile(join(directory, "highlights.scm"), "utf8"),
  ]);
  const query = new Query(language, source);
  const parser = new Parser();
  parser.setLanguage(language);
  return { parser, query };
}

export function highlightRanges(query: Query, tree: NonNullable<ReturnType<Parser["parse"]>>): Highlight[] {
  const ranges = query.captures(tree.rootNode).map(({ name, node }) => {
    if (!Object.hasOwn(captures, name)) {
      throw new Error(`No semantic token mapping for @${name}`);
    }
    const modifiers = name === "type.builtin" ? 1
      : name === "type.definition" || name === "function" ? 2 : 0;
    return {
      start: node.startIndex,
      end: node.endIndex,
      type: tokenTypes.indexOf(captures[name as CaptureName]),
      modifiers,
      priority: name === "function.call" || name === "type.builtin" ? 1 : 0,
    };
  });

  // The earliest range wins. For equal starts, favor the shorter range and
  // then the more specific capture (e.g. a method call over a field).
  ranges.sort((a, b) => a.start - b.start || a.end - b.end ||
    b.priority - a.priority || a.type - b.type || b.modifiers - a.modifiers);
  const selected: Highlight[] = [];
  let end = 0;
  for (const range of ranges) {
    if (range.start < end || range.end <= range.start) continue;
    selected.push(range);
    end = range.end;
  }
  return selected;
}

export function* splitLines(source: string, range: Highlight) {
  // web-tree-sitter 0.25 takes JS strings as UTF-16 and reports code-unit
  // indices. Use those indices directly and exclude line terminators.
  const content = source.slice(range.start, range.end);
  let start = range.start;
  for (const newline of content.matchAll(/\r\n|\r|\n/g)) {
    const end = range.start + newline.index;
    if (start < end) yield { ...range, start, end };
    start = end + newline[0].length;
  }
  if (start < range.end) yield { ...range, start };
}
