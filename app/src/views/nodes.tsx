// The node vocabulary shared by the two JSON-UI renderers: ViewRenderer.tsx
// (generative-UI tier, templates/CATALOG.md) and EventCardRenderer.tsx
// (constrained event-card tier, ADR-0013). Both draw `text`, `divider`,
// `keyValue`, `badge` and `status` under the same safety contract — text only
// (no HTML injection), unknown types degrade to a quiet placeholder, every tone
// resolves through tokens.ts — so those five plus the total-function prop
// accessors, the placeholder chip and the registry dispatch live here once.
//
// The tiers differ on two axes only, both carried by `NodeStyle`: how inline
// text is set, and density. Everything particular to a tier (the catalog's
// layout/table/form components, the card's eyebrow/optionList/submit/viewSlot/
// inset, each tier's heading scale) stays in its own file.
import { For, type JSX, Show } from "solid-js";
import type { ViewSpec } from "../protocol";
import { cn } from "@/lib/utils";
import { statusDotClass, toneBadgeClass, toneTextClass } from "./tokens";

// ---- Safe prop accessors (nothing here may throw on malformed input) ----

/** A catalog component node: an object with a string `type`. */
export type Node = ViewSpec;

export function isNode(x: unknown): x is Node {
  return typeof x === "object" && x !== null && typeof (x as { type?: unknown }).type === "string";
}

/** Child component nodes of a container, skipping any non-node entries. */
export function childNodes(node: Node): Node[] {
  const kids = node.children;
  return Array.isArray(kids) ? kids.filter(isNode) : [];
}

/** Render a catalog "display scalar" (string | number | boolean) as PLAIN text.
 * Never HTML — markdown/rich text is deferred to catalog v2, so this is the
 * "safe by vocabulary" boundary: agent-authored strings can never inject
 * markup. Nullish renders as empty. */
export function scalarText(v: unknown): string {
  if (v === null || v === undefined) return "";
  if (typeof v === "boolean") return v ? "true" : "false";
  return String(v);
}

export function str(v: unknown, fallback = ""): string {
  return typeof v === "string" ? v : fallback;
}

// ---- Inline text strategies ----

/** How a tier sets an inline string. Both are text-only; neither can emit
 * markup from the data. */
export type InlineText = (props: { text: unknown }) => JSX.Element;

/** The view tier: a display scalar, verbatim. */
export const PlainText: InlineText = (props) => <>{scalarText(props.text)}</>;

/** The event tier: the ONE text transform. `backtick` segments → mono spans.
 * Still text nodes only — prose can never *choose* mono, it only *marks*
 * machine tokens; the renderer decides the font. */
export const RichText: InlineText = (props) => {
  const parts = () => String(props.text ?? "").split("`");
  return (
    <For each={parts()}>
      {(part, i) =>
        part === "" ? null : i() % 2 === 1 ? (
          <span class="font-mono text-[0.92em] tracking-[-0.01em]">{part}</span>
        ) : (
          <>{part}</>
        )
      }
    </For>
  );
};

// ---- Degradation surface ----

/** The quiet dashed chip both tiers use for "something here we can't draw" and
 * for a spec that failed outright — honest, never a throw and never a blank. */
export function Notice(props: { children: JSX.Element }): JSX.Element {
  return (
    <div class="rounded-md border border-dashed border-border px-2.5 py-1.5 text-xs text-muted-foreground">
      {props.children}
    </div>
  );
}

/** Unknown/unsupported node → the notice chip naming the type. Keeps the tree
 * and its sibling content intact. */
export function UnsupportedNode(props: { label: string; type: string }): JSX.Element {
  return (
    <Notice>
      {props.label} <span class="font-mono">{props.type || "(untyped)"}</span>
    </Notice>
  );
}

// ---- Registry dispatch ----

/** Build a tier's recursive node renderer over its registry. Non-node values
 * render nothing; unknown types fall back to `unsupported(type)`. */
export function createNodeDispatch(
  registry: Record<string, (node: Node) => JSX.Element>,
  unsupported: (type: string) => JSX.Element,
): (props: { node: unknown }) => JSX.Element {
  return (props) => (
    <Show when={isNode(props.node)} fallback={null}>
      {(() => {
        const node = props.node as Node;
        const Comp = registry[node.type];
        return (
          <Show when={Comp} fallback={unsupported(node.type)}>
            {Comp!(node)}
          </Show>
        );
      })()}
    </Show>
  );
}

// ---- The shared node components ----

/** The two axes on which the tiers legitimately differ: inline text strategy,
 * and density. Every value is a COMPLETE literal class (Tailwind JIT). */
export interface NodeStyle {
  /** `PlainText` (view) or `RichText` (event card). */
  Text: InlineText;
  /** Leading class on a `text` paragraph: the view tier honours newlines
   * (`whitespace-pre-wrap`), the card caps the measure (`max-w-[68ch]`). */
  textLead: string;
  /** `keyValue` value type size: `text-sm` (view) / `text-xs` (card). */
  keyValueValue: string;
  /** `status` dot size: `size-2` (view) / `size-1.5` (card). */
  statusDot: string;
  /** Whether a `status` label carries its node's `tone` (card only). */
  statusLabelTone: boolean;
}

/** The node types both registries share, built for one tier's style. Spread the
 * result into the tier's registry. */
export function createSharedNodes(style: NodeStyle): Record<string, (node: Node) => JSX.Element> {
  const Text = style.Text;

  function TextNode(node: Node): JSX.Element {
    return (
      <p class={cn(style.textLead, "text-sm leading-relaxed text-foreground", toneTextClass(str(node.tone)))}>
        <Text text={node.text} />
      </p>
    );
  }

  function DividerNode(): JSX.Element {
    return <hr class="border-t border-border" />;
  }

  function KeyValueNode(node: Node): JSX.Element {
    const items = () => (Array.isArray(node.items) ? node.items : []);
    return (
      <dl class="flex flex-col gap-1.5">
        <For each={items()}>
          {(raw) => {
            const item = (raw ?? {}) as Record<string, unknown>;
            return (
              <div class="flex items-baseline justify-between gap-3">
                <dt class="shrink-0 text-xs text-muted-foreground">
                  <Text text={item.label} />
                </dt>
                <dd
                  class={cn(
                    "min-w-0 text-right",
                    style.keyValueValue,
                    "text-foreground",
                    toneTextClass(str(item.tone)),
                  )}
                >
                  <Text text={item.value} />
                </dd>
              </div>
            );
          }}
        </For>
      </dl>
    );
  }

  function BadgeNode(node: Node): JSX.Element {
    return (
      <span
        class={cn(
          "inline-flex w-fit items-center text-xs font-medium",
          toneBadgeClass(str(node.tone)),
        )}
      >
        <Text text={node.label} />
      </span>
    );
  }

  function StatusNode(node: Node): JSX.Element {
    const state = str(node.state, "neutral");
    return (
      <span class="inline-flex items-center gap-2 text-sm text-foreground" data-status={state}>
        <span
          class={cn(
            style.statusDot,
            "shrink-0 rounded-full",
            statusDotClass(state),
            state === "running" ? "motion-safe:animate-pulse" : "",
          )}
          aria-hidden="true"
        />
        <span class="text-xs text-muted-foreground">{state}</span>
        <Show
          when={style.statusLabelTone}
          fallback={
            <span>
              <Text text={node.label} />
            </span>
          }
        >
          <span class={toneTextClass(str(node.tone))}>
            <Text text={node.label} />
          </span>
        </Show>
      </span>
    );
  }

  return {
    text: TextNode,
    divider: DividerNode,
    keyValue: KeyValueNode,
    badge: BadgeNode,
    status: StatusNode,
  };
}
