import type { Element as HastElement, Root as HastRoot, RootContent } from "hast";
import { Check, Copy } from "@/components/ui/icons";
import { createMemo, createSignal, For, Show, Loading } from "solid-js";
import { type JSX } from "@solidjs/web";
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

export function CopyButton(props: { text: string }) {
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
 *
 * `wrap` trades the sideways scrollbar for wrapped lines — what a transcript
 * entry wants, where a horizontal scroll would hide most of the program.
 * `bare` drops the language label and the frame, leaving a tinted band for
 * callers that already carry a header and must not nest another border.
 */
export function CodeBlock(props: { code: string; lang?: string | null; wrap?: boolean; bare?: boolean }) {
  const tree = createMemo(async (): Promise<HastRoot | null> => {
    const code = props.code;
    const lang = props.lang ?? null;
    if (!resolveLanguage(lang)) return null;
    const { highlight } = await import("./highlight");
    return highlight(code, lang);
  });

  const body = () => (
    <code class="font-mono">
      <Loading fallback={props.code}><Show when={tree()} fallback={props.code}>
        {(highlighted) => <For each={highlighted().children}>{(node) => renderHast([node])}</For>}
      </Show></Loading>
    </code>
  );

  return (
    <div class="group relative flex flex-col gap-1">
      <div class={["flex items-center gap-2 pr-0.5", props.bare ? "justify-end" : "justify-between"]}>
        <Show when={!props.bare}>
          <span class="font-mono text-meta text-muted-foreground">{props.lang ?? "text"}</span>
        </Show>
        <CopyButton text={props.code} />
      </div>
      <pre
        class={[
          "px-2.5 py-2 text-xs leading-5",
          props.bare ? "rounded-md bg-muted/30" : "rounded-md border border-border/60",
          props.wrap ? "whitespace-pre-wrap wrap-break-word" : "overflow-x-auto",
        ]}
      >
        {body()}
      </pre>
    </div>
  );
}
