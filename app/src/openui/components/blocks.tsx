/** Presentational half of the OpenUI vocabulary: structure, prose and data.
 * Every renderer draws with the app's own tokens, so generated UI is on-brand
 * by construction rather than by asking a model to match a palette. */
import { createSignal, For, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Markdown as MarkdownBody } from "../../components/Markdown";
import { cn } from "@/lib/utils";
import type { OpenUiRenderer, RenderProps } from "../context";

const GAP: Record<string, string> = { none: "gap-0", sm: "gap-2", md: "gap-4", lg: "gap-6" };
const ALIGN: Record<string, string> = { start: "items-start", center: "items-center", end: "items-end", stretch: "items-stretch" };
const JUSTIFY: Record<string, string> = { start: "justify-start", center: "justify-center", end: "justify-end", between: "justify-between" };

export const Stack: OpenUiRenderer<{
  children?: unknown; direction?: string; gap?: string; align?: string; justify?: string; wrap?: boolean;
}> = p => <div class={cn("flex min-w-0", p.props.direction === "row" ? "flex-row [&>*]:min-w-0 [&>*]:flex-1" : "flex-col",
  GAP[p.props.gap ?? "md"] ?? GAP.md, ALIGN[p.props.align ?? ""] ?? "", JUSTIFY[p.props.justify ?? ""] ?? "", p.props.wrap ? "flex-wrap" : "")}
>{p.renderNode(p.props.children)}</div>;

export const Section: OpenUiRenderer<{ title?: string; children?: unknown; description?: string }> = p =>
  <section class="flex min-w-0 flex-col gap-3">
    <Show when={p.props.title}>{title => <div class="flex flex-col gap-0.5">
      <h2 class="text-sm font-semibold text-foreground">{title()}</h2>
      <Show when={p.props.description}><p class="text-meta text-muted-foreground">{p.props.description}</p></Show>
    </div>}</Show>
    {p.renderNode(p.props.children)}
  </section>;

export const Heading: OpenUiRenderer<{ text?: string; level?: number }> = p => {
  const size = () => (p.props.level === 1 ? "text-base font-semibold" : p.props.level === 3 ? "text-xs font-semibold uppercase tracking-wide text-muted-foreground" : "text-sm font-semibold");
  return <h3 class={cn("min-w-0 break-words text-foreground", size())}>{p.props.text}</h3>;
};

export const Text: OpenUiRenderer<{ text?: string; tone?: string; size?: string }> = p =>
  <p class={cn("min-w-0 break-words", p.props.size === "sm" ? "text-meta" : "text-sm",
    p.props.tone === "muted" ? "text-muted-foreground" : "text-foreground")}>{p.props.text}</p>;

export const Markdown: OpenUiRenderer<{ text?: string }> = p =>
  <Markdownish text={p.props.text ?? ""} />;
function Markdownish(props: { text: string }) { return <MarkdownBody class="min-w-0">{props.text}</MarkdownBody>; }

const TREND: Record<string, string> = { up: "text-status-success", down: "text-status-danger", flat: "text-muted-foreground" };
export const Metric: OpenUiRenderer<{ label?: string; value?: string; delta?: string; trend?: string }> = p =>
  <div data-slot="openui-metric" class="flex min-w-0 flex-col gap-0.5 rounded-lg border border-border px-3 py-2">
    <span class="text-meta text-muted-foreground">{p.props.label}</span>
    <span class="text-base font-semibold tabular-nums text-foreground">{p.props.value}</span>
    <Show when={p.props.delta}><span class={cn("text-meta tabular-nums", TREND[p.props.trend ?? "flat"] ?? TREND.flat)}>{p.props.delta}</span></Show>
  </div>;

export const Card: OpenUiRenderer<{ children?: unknown; title?: string; description?: string }> = p =>
  <div data-slot="openui-card" class="flex min-w-0 flex-col gap-3 rounded-lg border border-border bg-card p-4">
    <Show when={p.props.title}>{title => <div class="flex flex-col gap-0.5">
      <span class="text-sm font-semibold text-foreground">{title()}</span>
      <Show when={p.props.description}><span class="text-meta text-muted-foreground">{p.props.description}</span></Show>
    </div>}</Show>
    {p.renderNode(p.props.children)}
  </div>;

const CALLOUT: Record<string, string> = {
  info: "border-border text-foreground",
  success: "border-status-success/40 text-foreground",
  warning: "border-status-attention/50 text-foreground",
  danger: "border-status-danger/50 text-foreground",
};
export const Callout: OpenUiRenderer<{ text?: string; variant?: string; title?: string }> = p =>
  <div role={p.props.variant === "danger" ? "alert" : undefined} data-slot="openui-callout" data-variant={p.props.variant ?? "info"}
    class={cn("flex min-w-0 flex-col gap-1 rounded-lg border-l-2 bg-muted/40 px-3 py-2", CALLOUT[p.props.variant ?? "info"] ?? CALLOUT.info)}>
    <Show when={p.props.title}><span class="text-sm font-semibold">{p.props.title}</span></Show>
    <span class="text-sm">{p.props.text}</span>
  </div>;

export const Table: OpenUiRenderer<{ columns?: unknown; rows?: unknown; caption?: string }> = p => {
  const columns = () => asStrings(p.props.columns);
  const rows = () => (Array.isArray(p.props.rows) ? p.props.rows.map(asStrings) : []);
  return <div class="min-w-0 overflow-x-auto"><table data-slot="openui-table" class="w-full border-collapse text-sm">
    <Show when={p.props.caption}><caption class="pb-2 text-left text-meta text-muted-foreground">{p.props.caption}</caption></Show>
    <thead><tr><For each={columns()}>{column => <th scope="col" class="border-b border-border px-2 py-1.5 text-left text-meta font-medium text-muted-foreground">{column}</th>}</For></tr></thead>
    <tbody><For each={rows()}>{row => <tr><For each={row}>{cell => <td class="border-b border-border/60 px-2 py-1.5 tabular-nums">{cell}</td>}</For></tr>}</For></tbody>
  </table></div>;
};

export const List: OpenUiRenderer<{ items?: unknown; ordered?: boolean }> = p =>
  <Show when={p.props.ordered} fallback={<ul class="flex min-w-0 flex-col gap-1">{p.renderNode(p.props.items)}</ul>}>
    <ol class="flex min-w-0 list-inside list-decimal flex-col gap-1">{p.renderNode(p.props.items)}</ol>
  </Show>;
export const ListItem: OpenUiRenderer<{ text?: string; detail?: string }> = p =>
  <li class="flex min-w-0 items-baseline justify-between gap-3 text-sm">
    <span class="min-w-0 break-words">{p.props.text}</span>
    <Show when={p.props.detail}><span class="shrink-0 text-meta tabular-nums text-muted-foreground">{p.props.detail}</span></Show>
  </li>;

export const Image: OpenUiRenderer<{ src?: string; alt?: string; caption?: string }> = p =>
  <figure class="flex min-w-0 flex-col gap-1">
    <img src={p.props.src} alt={p.props.alt ?? ""} loading="lazy" class="max-w-full rounded-lg border border-border" />
    <Show when={p.props.caption}><figcaption class="text-meta text-muted-foreground">{p.props.caption}</figcaption></Show>
  </figure>;

export const Separator: OpenUiRenderer<Record<string, never>> = () => <hr class="my-1 border-0 border-t border-border" />;

export const CodeBlock: OpenUiRenderer<{ code?: string; language?: string }> = p =>
  <pre data-slot="openui-code" data-language={p.props.language} class="min-w-0 overflow-x-auto rounded-lg border border-border bg-muted/40 p-3 font-mono text-xs"><code>{p.props.code}</code></pre>;

export const TabItem: OpenUiRenderer<{ value?: string; trigger?: string; content?: unknown }> = p =>
  <div>{p.renderNode(p.props.content)}</div>;

/** Tabs owns the selected panel itself: its children arrive as parsed TabItem
 * elements, so it reads their props rather than rendering all of them. */
export const Tabs = (p: RenderProps<{ items?: unknown }>): JSX.Element => {
  const items = () => (Array.isArray(p.props.items) ? p.props.items.filter(isElement) : []);
  const [selected, setSelected] = createSignal(0);
  const current = () => Math.min(selected(), Math.max(items().length - 1, 0));
  return <div class="flex min-w-0 flex-col gap-3">
    <div role="tablist" class="flex flex-wrap gap-1 border-b border-border">
      <For each={items()}>{(item, index) => <button type="button" role="tab" aria-selected={current() === index() ? "true" : "false"}
        class="min-h-11 px-2.5 text-xs font-medium text-muted-foreground transition-colors hover:text-foreground aria-selected:border-b-2 aria-selected:border-primary aria-selected:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        onClick={() => setSelected(index())}>{String(item.props?.["trigger"] ?? item.props?.["value"] ?? `Tab ${index() + 1}`)}</button>}</For>
    </div>
    <div role="tabpanel">{p.renderNode(items()[current()]?.props?.["content"])}</div>
  </div>;
};

export const FollowUp = (p: RenderProps<{ suggestions?: unknown }>): JSX.Element =>
  <div data-slot="openui-followup" class="flex min-w-0 flex-wrap gap-2">
    <For each={asStrings(p.props.suggestions)}>{suggestion => <button type="button" disabled={p.ctx.streaming()}
      class="min-h-11 rounded-full border border-border px-3 text-xs text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      onClick={() => p.ctx.trigger({ message: suggestion, action: "follow_up", params: { suggestion } })}>{suggestion}</button>}</For>
  </div>;

function asStrings(value: unknown): string[] {
  return Array.isArray(value) ? value.map(item => (item == null ? "" : String(item))) : [];
}
function isElement(value: unknown): value is { props?: Record<string, unknown> } {
  return typeof value === "object" && value !== null && (value as { type?: string }).type === "element";
}
