import { For, Show, type JSX } from "solid-js";

// Minimal, dependency-free, XSS-safe markdown for Chat messages and Inbox Item
// content (ports lashapp/frontend/src/components/Markdown.tsx). It renders a
// useful subset (fenced code blocks, inline `code`, **bold**, *italic*, links,
// "- " bullet lists) by building real DOM nodes - never innerHTML. This drops
// react-markdown + remark-gfm entirely (a large bundle win vs the React app).

type Block =
  | { kind: "code"; lang?: string; text: string }
  | { kind: "list"; items: string[] }
  | { kind: "para"; text: string };

function parseBlocks(source: string): Block[] {
  const lines = source.replace(/\r\n/g, "\n").split("\n");
  const blocks: Block[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    const fence = line.match(/^```(\w+)?\s*$/);
    if (fence) {
      const lang = fence[1];
      const body: string[] = [];
      i += 1;
      while (i < lines.length && !/^```\s*$/.test(lines[i])) {
        body.push(lines[i]);
        i += 1;
      }
      i += 1; // closing fence
      blocks.push({ kind: "code", lang, text: body.join("\n") });
      continue;
    }
    if (/^\s*[-*]\s+/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^\s*[-*]\s+/.test(lines[i])) {
        items.push(lines[i].replace(/^\s*[-*]\s+/, ""));
        i += 1;
      }
      blocks.push({ kind: "list", items });
      continue;
    }
    if (line.trim() === "") {
      i += 1;
      continue;
    }
    const para: string[] = [];
    while (
      i < lines.length &&
      lines[i].trim() !== "" &&
      !/^```/.test(lines[i]) &&
      !/^\s*[-*]\s+/.test(lines[i])
    ) {
      para.push(lines[i]);
      i += 1;
    }
    blocks.push({ kind: "para", text: para.join("\n") });
  }
  return blocks;
}

// Inline: `code`, **bold**, *italic*, [text](http url). Returns JSX nodes.
function renderInline(text: string): JSX.Element[] {
  const nodes: JSX.Element[] = [];
  const pattern = /(`[^`]+`)|(\*\*[^*]+\*\*)|(\*[^*]+\*)|(\[[^\]]+\]\((https?:\/\/[^\s)]+)\))/g;
  let last = 0;
  let match: RegExpExecArray | null;
  const pushText = (value: string) => {
    const parts = value.split("\n");
    parts.forEach((part, index) => {
      if (index > 0) nodes.push(<br />);
      if (part) nodes.push(part);
    });
  };
  while ((match = pattern.exec(text)) !== null) {
    if (match.index > last) pushText(text.slice(last, match.index));
    const token = match[0];
    if (token.startsWith("`")) {
      nodes.push(
        <code class="rounded bg-muted/70 px-1 py-0.5 font-mono text-[0.85em]">
          {token.slice(1, -1)}
        </code>,
      );
    } else if (token.startsWith("**")) {
      nodes.push(<strong>{token.slice(2, -2)}</strong>);
    } else if (token.startsWith("*")) {
      nodes.push(<em>{token.slice(1, -1)}</em>);
    } else {
      const label = token.slice(1, token.indexOf("]"));
      const href = match[5];
      nodes.push(
        <a
          href={href}
          target="_blank"
          rel="noreferrer"
          class="underline decoration-current/50 underline-offset-2 hover:decoration-current"
        >
          {label}
        </a>,
      );
    }
    last = pattern.lastIndex;
  }
  if (last < text.length) pushText(text.slice(last));
  return nodes;
}

export function Markdown(props: { children: string; class?: string }) {
  const blocks = () => parseBlocks(props.children);
  return (
    <div
      data-testid="markdown"
      class={`grid gap-2 text-sm leading-relaxed wrap-break-word ${props.class ?? ""}`}
    >
      <For each={blocks()}>
        {(block) => (
          <Show
            when={block.kind !== "code"}
            fallback={
              <pre class="overflow-auto rounded-md border border-border bg-black/20 p-2.5 text-xs leading-5">
                <code class="font-mono">{(block as { text: string }).text}</code>
              </pre>
            }
          >
            <Show
              when={block.kind === "list"}
              fallback={<p>{renderInline((block as { text: string }).text)}</p>}
            >
              <ul class="ml-4 grid list-disc gap-1">
                <For each={(block as { items: string[] }).items}>
                  {(item) => <li>{renderInline(item)}</li>}
                </For>
              </ul>
            </Show>
          </Show>
        )}
      </For>
    </div>
  );
}
