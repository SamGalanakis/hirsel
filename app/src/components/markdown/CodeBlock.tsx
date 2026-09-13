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
      class="inline-flex min-h-5 items-center gap-1 rounded px-1 text-meta text-muted-foreground opacity-0 transition-opacity hover:text-foreground focus-visible:opacity-100 group-hover:opacity-100 pointer-coarse:opacity-100"
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
 * A fenced code block: one frame, with the language label and the copy
 * affordance together on its top edge — a caption inside the thing it names,
 * not a loose word floating above it — and highlighting that lazy-loads. Plain
 * mono text paints first and is replaced in place once the highlighter chunk
 * resolves, so nothing blocks the message.
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
    <div class={["group relative flex flex-col rounded-md", props.bare ? "bg-muted/30" : "border border-border/60"]}>
      <div class="flex items-center justify-end gap-2 px-1.5 pt-1">
        <Show when={!props.bare}>
          <span class="font-mono text-meta text-muted-foreground">{props.lang ?? "text"}</span>
        </Show>
        <CopyButton text={props.code} />
      </div>
      <pre
        class={[
          "rounded-md px-2.5 pb-2 text-xs",
          // The same edge fade a wide table wears: the app's one "there is more
          // this way" cue, instead of a bare clipped edge.
          props.wrap ? "whitespace-pre-wrap wrap-break-word" : "scroll-fade-x overflow-x-auto",
        ]}
      >
        {body()}
      </pre>
    </div>
  );
}
