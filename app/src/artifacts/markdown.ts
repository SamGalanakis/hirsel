import type { Nodes } from "mdast";
import { parseMarkdown, mdastToString } from "../components/markdown/parse";
import { safeHref } from "../components/markdown/url";
import type { Artifact } from "./types";

export function escapeHtml(source: string): string {
  return source.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;");
}
export function isMarkdownArtifact(artifact: Artifact): boolean {
  return artifact.kind === "file" && (artifact.mime.split(";", 1)[0].trim().toLowerCase() === "text/markdown" || /\.(?:md|markdown)$/i.test(artifact.filename ?? ""));
}
/** Same CommonMark/GFM parser and URL policy as conversation Markdown. Only
 * explicit safe tags become markup; raw HTML is literal, and references have
 * no host navigation or tool capability in the isolated document. */
function render(node: Nodes, definitions: Map<string, string>, header = false): string {
  const children = () => "children" in node ? node.children.map(child => render(child, definitions)).join("") : "";
  const wrap = (tag: string) => `<${tag}>${children()}</${tag}>`;
  switch (node.type) {
    case "root": return children();
    case "text": case "html": return escapeHtml(node.value);
    case "paragraph": return wrap("p");
    case "heading": return wrap(`h${node.depth}`);
    case "strong": return wrap("strong");
    case "emphasis": return wrap("em");
    case "delete": return wrap("del");
    case "blockquote": return wrap("blockquote");
    case "break": return "<br>";
    case "thematicBreak": return "<hr>";
    case "inlineCode": return `<code>${escapeHtml(node.value)}</code>`;
    case "code": return `<pre><code>${escapeHtml(node.value)}</code></pre>`;
    case "list": return node.ordered ? `<ol start="${node.start ?? 1}">${children()}</ol>` : wrap("ul");
    case "listItem": return node.checked === null || node.checked === undefined ? wrap("li")
      : `<li class="task-list-item"><input type="checkbox" disabled${node.checked ? " checked" : ""} aria-label="${node.checked ? "Complete" : "Incomplete"}"><div>${children()}</div></li>`;
    case "link": return link(node.url, children());
    case "linkReference": return link(definitions.get(node.identifier.toLowerCase()), children());
    case "definition": return "";
    case "image": return escapeHtml(node.alt ?? "");
    case "table": return `<div class="table-scroll"><table><thead>${render(node.children[0], definitions, true)}</thead><tbody>${node.children.slice(1).map(row => render(row, definitions)).join("")}</tbody></table></div>`;
    case "tableRow": return `<tr>${node.children.map(cell => render(cell, definitions, header)).join("")}</tr>`;
    case "tableCell": return wrap(header ? "th" : "td");
    default: return escapeHtml(mdastToString(node));
  }
}
function link(url: string | undefined, label: string): string {
  const href = safeHref(url);
  if (!href) return label;
  // Opaque documents cannot safely navigate out of their dismissal environment.
  return `<span class="document-link">${label} <span class="link-url">(${escapeHtml(href)})</span></span>`;
}
export function markdownDocumentBody(source: string): string {
  const root = parseMarkdown(source);
  const definitions = new Map(root.children.filter(node => node.type === "definition").map(node => [node.identifier.toLowerCase(), node.url]));
  return `<article class="markdown">${render(root, definitions)}</article>`;
}
export const MARKDOWN_STYLE = ".markdown{max-width:75ch;margin:auto}.markdown h1,.markdown h2,.markdown h3{line-height:1.25}.markdown h1{font-size:1.75rem}.markdown h2{font-size:1.4rem}.markdown h3{font-size:1.15rem}.markdown pre{padding:1rem;background:#f1f5f4;border-radius:8px;overflow:auto}.markdown code{font-size:.9em}.document-link{color:#247463}.link-url{font-size:.85em;overflow-wrap:anywhere}.markdown blockquote{margin-left:0;padding-left:1rem;border-left:3px solid #cbd5d1}.markdown table{border-collapse:collapse}.markdown td,.markdown th{border:1px solid #cbd5d1;padding:.5rem}.table-scroll{overflow:auto}.markdown li>p,.markdown .task-list-item>div>p{margin:.25rem 0}.markdown .task-list-item{display:grid;grid-template-columns:1em minmax(0,1fr);column-gap:.5em;list-style:none}.markdown .task-list-item>input{width:1em;height:1em;margin:.25em 0 0}.markdown .task-list-item>div>:first-child{margin-top:0}";
