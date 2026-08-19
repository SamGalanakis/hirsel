import type { Root } from "hast";

// Syntax highlighting lives in its own lazily imported chunk so it never blocks
// first paint: code renders as plain mono text immediately and upgrades when
// this module (and highlight.js' grammars) arrive.
//
// lowlight (highlight.js) over shiki deliberately: shiki's smallest viable
// client build still ships a regex engine plus TextMate grammars (hundreds of
// KB), while a hand-picked highlight.js set is an order of magnitude smaller
// and, like shiki, yields a hast tree — so we still build real DOM nodes and
// never touch innerHTML.

/** Languages worth carrying for agent output. Everything else renders plain. */
const grammars = {
  bash: () => import("highlight.js/lib/languages/bash"),
  c: () => import("highlight.js/lib/languages/c"),
  css: () => import("highlight.js/lib/languages/css"),
  diff: () => import("highlight.js/lib/languages/diff"),
  go: () => import("highlight.js/lib/languages/go"),
  ini: () => import("highlight.js/lib/languages/ini"),
  java: () => import("highlight.js/lib/languages/java"),
  javascript: () => import("highlight.js/lib/languages/javascript"),
  json: () => import("highlight.js/lib/languages/json"),
  markdown: () => import("highlight.js/lib/languages/markdown"),
  python: () => import("highlight.js/lib/languages/python"),
  rust: () => import("highlight.js/lib/languages/rust"),
  sql: () => import("highlight.js/lib/languages/sql"),
  typescript: () => import("highlight.js/lib/languages/typescript"),
  xml: () => import("highlight.js/lib/languages/xml"),
  yaml: () => import("highlight.js/lib/languages/yaml"),
} as const;

type GrammarName = keyof typeof grammars;

/** Fence info strings agents actually write, mapped onto the set above. */
const aliases: Record<string, GrammarName> = {
  bash: "bash",
  c: "c",
  cpp: "c",
  console: "bash",
  css: "css",
  diff: "diff",
  go: "go",
  html: "xml",
  ini: "ini",
  java: "java",
  javascript: "javascript",
  js: "javascript",
  json: "json",
  jsonc: "json",
  jsx: "javascript",
  kotlin: "java",
  markdown: "markdown",
  md: "markdown",
  patch: "diff",
  python: "python",
  py: "python",
  rs: "rust",
  rust: "rust",
  sh: "bash",
  shell: "bash",
  sql: "sql",
  svg: "xml",
  toml: "ini",
  ts: "typescript",
  tsx: "typescript",
  typescript: "typescript",
  xml: "xml",
  yaml: "yaml",
  yml: "yaml",
  zsh: "bash",
};

/** The grammar we would use for a fence's info string, if any. */
export function resolveLanguage(lang: string | null | undefined): GrammarName | null {
  if (!lang) return null;
  return aliases[lang.trim().toLowerCase()] ?? null;
}

type Lowlight = ReturnType<typeof import("lowlight").createLowlight>;

let lowlightPromise: Promise<Lowlight> | null = null;
const registered = new Set<GrammarName>();

async function getLowlight() {
  if (!lowlightPromise) {
    lowlightPromise = import("lowlight").then(({ createLowlight }) => createLowlight());
  }
  return lowlightPromise;
}

/**
 * Highlight `code` as `lang`, returning a hast tree the renderer walks into
 * real spans. Resolves to `null` when the language isn't one we carry.
 */
export async function highlight(code: string, lang: string | null | undefined): Promise<Root | null> {
  const name = resolveLanguage(lang);
  if (!name) return null;
  const lowlight = await getLowlight();
  if (!registered.has(name)) {
    const module = await grammars[name]();
    lowlight.register(name, module.default);
    registered.add(name);
  }
  return lowlight.highlight(name, code) as Root;
}
