import type { PhrasingContent, RootContent, Table } from "mdast";
import { createMemo, For, Show, type JSX } from "solid-js";
import { Dynamic } from "solid-js/web";
import { CodeBlock } from "./markdown/CodeBlock";
import { mdastToString, parseMarkdown, parseStreamingMarkdown } from "./markdown/parse";

// CommonMark + GFM rendering for task conversation content. The source is
// parsed to mdast (micromark under the hood) and mapped to Solid JSX here, so
// every node in the output is one we chose to build: `html` nodes are dropped,
// URLs are scheme-checked, and nothing anywhere assigns innerHTML. That keeps
// the app's XSS-safe-by-construction property without a sanitizer.

const SAFE_SCHEMES = ["http:", "https:", "mailto:"];

/** A safe href, or null when the URL should degrade to plain text. */
function safeHref(url: string | null | undefined): string | null {
  if (!url) return null;
  const trimmed = url.trim();
  // Relative and anchor links carry no scheme and are harmless.
  if (/^[#/](?![/\\])/.test(trimmed)) return trimmed;
  try {
    const parsed = new URL(trimmed, "https://hirsel.invalid/");
    return SAFE_SCHEMES.includes(parsed.protocol) ? trimmed : null;
  } catch {
    return null;
  }
}

const inlineCodeClass = "rounded bg-muted/70 px-1 py-0.5 font-mono text-[0.85em]";
const linkClass = "underline decoration-current/50 underline-offset-2 hover:decoration-current";

function renderPhrasing(nodes: readonly PhrasingContent[]): JSX.Element[] {
  const out: JSX.Element[] = [];
  for (const node of nodes) {
    switch (node.type) {
      case "text":
        out.push(node.value);
        break;
      case "inlineCode":
        out.push(<code class={inlineCodeClass}>{node.value}</code>);
        break;
      case "strong":
        out.push(<strong class="font-medium text-foreground">{renderPhrasing(node.children)}</strong>);
        break;
      case "emphasis":
        out.push(<em>{renderPhrasing(node.children)}</em>);
        break;
      case "delete":
        out.push(<del class="text-muted-foreground">{renderPhrasing(node.children)}</del>);
        break;
      case "break":
        out.push(<br />);
        break;
      case "link": {
        const href = safeHref(node.url);
        out.push(
          href ? (
            <a
              href={href}
              target="_blank"
              rel="noopener noreferrer nofollow"
              title={node.title ?? undefined}
              class={linkClass}
            >
              {renderPhrasing(node.children)}
            </a>
          ) : (
            <>{renderPhrasing(node.children)}</>
          ),
        );
        break;
      }
      case "image": {
        const src = safeHref(node.url);
        out.push(
          src ? (
            <img
              src={src}
              alt={node.alt ?? ""}
              title={node.title ?? undefined}
              loading="lazy"
              class="max-w-full rounded-md border border-border/60"
            />
          ) : (
            <>{node.alt ?? ""}</>
          ),
        );
        break;
      }
      case "linkReference":
      case "footnoteReference":
        out.push(<>{mdastToString(node)}</>);
        break;
      case "imageReference":
        out.push(node.alt ?? "");
        break;
      case "html":
        // Raw HTML never becomes markup — show the author's literal text.
        out.push(node.value);
        break;
      default:
        out.push(<>{mdastToString(node)}</>);
    }
  }
  return out;
}

const headingClass: Record<number, string> = {
  1: "mt-1 text-[1.05rem] font-medium leading-snug text-foreground",
  2: "mt-1 text-[0.95rem] font-medium leading-snug text-foreground",
  3: "mt-1 text-sm font-medium leading-snug text-foreground",
};

function Heading(props: { depth: number; children: JSX.Element }) {
  // h4-h6 keep their semantics but share h3's restrained scale (DESIGN.md:
  // no oversized section headings in conversation).
  return (
    <Dynamic component={`h${props.depth}`} class={headingClass[props.depth] ?? headingClass[3]}>
      {props.children}
    </Dynamic>
  );
}

function alignClass(align: Table["align"], index: number): string {
  const value = align?.[index];
  if (value === "center") return "text-center";
  if (value === "right") return "text-right";
  return "text-left";
}

function TableBlock(props: { node: Table }) {
  const rows = () => props.node.children;
  return (
    // Wide tables scroll inside their own box; the message column never widens.
    <div class="overflow-x-auto">
      <table class="w-max min-w-full border-collapse text-sm">
        <thead>
          <For each={rows().slice(0, 1)}>
            {(row) => (
              <tr>
                <For each={row.children}>
                  {(cell, index) => (
                    <th
                      class={`border-b border-border/60 px-2 py-1 font-medium text-muted-foreground ${alignClass(props.node.align, index())}`}
                    >
                      {renderPhrasing(cell.children)}
                    </th>
                  )}
                </For>
              </tr>
            )}
          </For>
        </thead>
        <tbody>
          <For each={rows().slice(1)}>
            {(row) => (
              <tr>
                <For each={row.children}>
                  {(cell, index) => (
                    <td
                      class={`border-b border-border/30 px-2 py-1 align-top ${alignClass(props.node.align, index())}`}
                    >
                      {renderPhrasing(cell.children)}
                    </td>
                  )}
                </For>
              </tr>
            )}
          </For>
        </tbody>
      </table>
    </div>
  );
}

/** A list item's content, with the leading paragraph unwrapped so tight items
 * sit on the marker line and nested lists still nest as blocks. */
function renderListItemBody(children: readonly RootContent[]): JSX.Element[] {
  const [first, ...rest] = children;
  if (first?.type !== "paragraph") return renderNodes(children);
  return [...renderPhrasing(first.children), ...renderNodes(rest)];
}

function ListBlock(props: { node: Extract<RootContent, { type: "list" }> }) {
  const items = () => props.node.children;
  const itemClass = "marker:text-muted-foreground";
  const body = (
    <For each={items()}>
      {(item) => (
        <li
          class={item.checked === null || item.checked === undefined ? itemClass : `${itemClass} list-none -ml-4`}
        >
          <Show when={item.checked !== null && item.checked !== undefined}>
            <input
              type="checkbox"
              checked={item.checked === true}
              disabled
              class="mr-1.5 align-middle accent-primary"
              aria-hidden="true"
            />
          </Show>
          {renderListItemBody(item.children)}
        </li>
      )}
    </For>
  );
  return (
    <Show
      when={props.node.ordered}
      fallback={<ul class="ml-4 grid list-disc gap-1">{body}</ul>}
    >
      <ol class="ml-4 grid list-decimal gap-1" start={props.node.start ?? 1}>
        {body}
      </ol>
    </Show>
  );
}

function renderNodes(nodes: readonly RootContent[]): JSX.Element[] {
  const out: JSX.Element[] = [];
  for (const node of nodes) {
    switch (node.type) {
      case "paragraph":
        out.push(<p>{renderPhrasing(node.children)}</p>);
        break;
      case "heading":
        out.push(<Heading depth={node.depth}>{renderPhrasing(node.children)}</Heading>);
        break;
      case "code":
        out.push(<CodeBlock code={node.value} lang={node.lang} />);
        break;
      case "list":
        out.push(<ListBlock node={node} />);
        break;
      case "blockquote":
        // Same zero-floor track as the block root: a quoted table or code
        // block must scroll inside the quote, not widen it.
        out.push(
          <blockquote class="grid grid-cols-[minmax(0,1fr)] gap-2 border-l border-border/60 pl-2.5 text-muted-foreground">
            {renderNodes(node.children)}
          </blockquote>,
        );
        break;
      case "thematicBreak":
        out.push(<hr class="border-border/60" />);
        break;
      case "table":
        out.push(<TableBlock node={node} />);
        break;
      case "html":
        // Literal text, never markup.
        out.push(<p class="wrap-break-word">{node.value}</p>);
        break;
      case "definition":
      case "footnoteDefinition":
        break;
      default:
        out.push(<p>{mdastToString(node)}</p>);
    }
  }
  return out;
}

/**
 * Reduce inline markdown to its plain text, keeping the content. For
 * single-line contexts that can't host rich inline nodes — notably the shimmer
 * status marker, whose `background-clip:text` treatment breaks on nested
 * styled/backgrounded spans — so the live line never shows literal
 * `**asterisks**` yet keeps shimmering.
 */
export function stripInlineMarkdown(text: string): string {
  return mdastToString(parseMarkdown(text));
}

/** Inline markdown as JSX nodes, for one-line contexts (reasoning, status). */
export function renderInline(text: string): JSX.Element[] {
  const root = parseStreamingMarkdown(text);
  const out: JSX.Element[] = [];
  root.children.forEach((node, index) => {
    if (index > 0) out.push(<br />);
    if (node.type === "paragraph" || node.type === "heading") out.push(...renderPhrasing(node.children));
    else out.push(...renderNodes([node]));
  });
  return out;
}

export function Markdown(props: { children: string; class?: string }) {
  // One parse per source change keeps streaming updates cheap and stable.
  const tree = createMemo(() => parseStreamingMarkdown(props.children));
  return (
    <div
      data-testid="markdown"
      /* The column is `minmax(0,1fr)` rather than the implicit `auto` track.
         An auto track is floored at its item's min-content width, so a wide
         table or an unbreakable command line in a code block widened this
         block — and every ancestor with it — past the viewport, where the app
         shell's `overflow: hidden` simply cut it off with nothing to scroll.
         With a zero floor the block stays at its parent's width and the
         `overflow-x-auto` boxes inside it do the scrolling they were written
         to do. */
      class={`grid grid-cols-[minmax(0,1fr)] gap-2 text-sm leading-relaxed wrap-break-word ${props.class ?? ""}`}
    >
      <For each={tree().children}>{(node) => renderNodes([node])}</For>
    </div>
  );
}
