import type { Element as HastElement, Root as HastRoot, RootContent } from "hast";
import { Check, Copy } from "lucide-solid";
import { createResource, createSignal, For, Show, type JSX } from "solid-js";
import { resolveLanguage } from "./highlight";

function hastClass(node: HastElement): string | undefined {
  const value = node.properties?.className;
  if (Array.isArray(value)) return value.join(" ");
  return typeof value === "string" ? value : undefined;
}

/** hast -> real DOM nodes. Only `element` and `text` are honoured, so a
 * highlighter can never introduce markup we didn't ask for. */
function renderHast(nodes: readonly RootContent[]): JSX.Element[] {
  const out: JSX.Element[] = [];
  for (const node of nodes) {
    if (node.type === "text") out.push(node.value);
    else if (node.type === "element" && node.tagName === "span")
      out.push(<span class={hastClass(node)}>{renderHast(node.children)}</span>);
    else if (node.type === "element") out.push(...renderHast(node.children));
  }
  return out;
}

function CopyButton(props: { text: string }) {
  const [copied, setCopied] = createSignal(false);
  const copy = async () => {
    try {
      await navigator.clipboard?.writeText(props.text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1400);
    } catch {
      // Clipboard denied or unavailable: leave the affordance silent.
    }
  };
  return (
    <button
      type="button"
      class="inline-flex items-center gap-1 rounded px-1 py-px text-meta text-muted-foreground opacity-0 transition-opacity hover:text-foreground focus-visible:opacity-100 group-hover:opacity-100"
      aria-label={copied() ? "Copied" : "Copy code"}
      onClick={copy}
    >
      <Show when={copied()} fallback={<Copy class="size-3" aria-hidden="true" />}>
        <Check class="size-3 text-status-success" aria-hidden="true" />
      </Show>
      {copied() ? "Copied" : "Copy"}
    </button>
  );
}

/**
 * A fenced code block: language label, copy affordance, and highlighting that
 * lazy-loads. Plain mono text paints first and is replaced in place once the
 * highlighter chunk resolves, so nothing blocks the message.
 */
export function CodeBlock(props: { code: string; lang?: string | null }) {
  const [tree] = createResource(
    () => ({ code: props.code, lang: props.lang ?? null }),
    async (input): Promise<HastRoot | null> => {
      if (!resolveLanguage(input.lang)) return null;
      const { highlight } = await import("./highlight");
      return highlight(input.code, input.lang);
    },
  );

  return (
    <div class="group relative flex flex-col gap-1">
      <div class="flex items-center justify-between gap-2 pr-0.5">
        <span class="font-mono text-meta text-muted-foreground">{props.lang ?? "text"}</span>
        <CopyButton text={props.code} />
      </div>
      <pre class="overflow-x-auto rounded-md border border-border/60 px-2.5 py-2 text-xs leading-5">
        <code class="font-mono">
          <Show when={tree()} fallback={props.code}>
            {(highlighted) => <For each={highlighted().children}>{(node) => renderHast([node])}</For>}
          </Show>
        </code>
      </pre>
    </div>
  );
}
