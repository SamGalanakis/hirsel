import type { Root } from "mdast";
import { fromMarkdown } from "mdast-util-from-markdown";
import { gfmFromMarkdown } from "mdast-util-gfm";
import { toString as mdastToString } from "mdast-util-to-string";
import { gfm } from "micromark-extension-gfm";
import remend from "remend";

// CommonMark + GFM parsing for agent prose. We call micromark/mdast directly
// rather than through unified + remark-parse: the pipeline machinery (unified,
// vfile, trough) buys nothing here and costs bundle, and `fromMarkdown` is what
// remark-parse wraps anyway.
//
// Nothing downstream ever touches innerHTML: the renderer walks this tree and
// builds real DOM nodes, and `html` nodes are rendered as their literal text
// rather than markup, so the output is sanitized by construction with no
// sanitizer dependency.

const micromarkExtensions = [gfm()];
const mdastExtensions = [gfmFromMarkdown()];

/** Parse markdown source (already stream-healed) into an mdast tree. */
export function parseMarkdown(source: string): Root {
  return fromMarkdown(source, { extensions: micromarkExtensions, mdastExtensions });
}

/**
 * Heal a partially streamed message so half-typed syntax doesn't flash as
 * broken markup: `**bol` renders bold, an unterminated fence renders as code,
 * a half-typed `[label](htt` renders as its label. Streamdown's `remend`
 * (zero-dep, string in / string out) does the completion.
 */
export function healStreamingMarkdown(source: string): string {
  return remend(source, { linkMode: "text-only" });
}

/** Parse + heal in one step — what the renderer uses. */
export function parseStreamingMarkdown(source: string): Root {
  return parseMarkdown(healStreamingMarkdown(source));
}

export { mdastToString };
