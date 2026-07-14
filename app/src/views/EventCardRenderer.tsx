// The interactive JSON-UI event-card renderer (ADR-0013) — the interactive
// sibling of ViewRenderer.tsx. It renders the CONSTRAINED event vocabulary
// (eyebrow · heading · text · keyValue · badge · status · divider · optionList ·
// field · submit · viewSlot · inset) onto DESIGN.md tokens, and degrades any
// unknown node to a quiet fallback chip — never throws, never blanks, never
// loses content. The nodes carry SEMANTIC tokens only (`tone`, `state`,
// `recommended`, `boundary`); the renderer alone owns the palette and the type
// scale, so a machine-authored card is on-brand by construction — it cannot
// express a raw color, a second accent, a glow, or type above the 16px ceiling.
//
// Text is set as text (no HTML injection); the ONLY transform is `` `backtick` ``
// → monospace, so machine tokens get mono and prose never can (Monospace-Earns-
// It, enforced by the renderer, not the data).
//
// Interactions post back through `onAction(action, data)` — the caller turns
// that into an `event_action` frame (see lib/event-decide.ts). optionList emits
// `choose {choice, label}`; submit collects the card's field values and emits
// `{…fields}`. This mirrors ViewRenderer's emit context, scoped to one card.
import {
  createContext,
  createSignal,
  ErrorBoundary,
  For,
  Show,
  useContext,
} from "solid-js";
import type { JSX } from "solid-js";
import type { ViewSpec } from "../protocol";
import { eventUiNodes } from "../store/selectors";
import { cn } from "@/lib/utils";
import { eyebrowToneClass, statusDotClass, toneBadgeClass, toneTextClass } from "./tokens";

type Node = ViewSpec;

function isNode(x: unknown): x is Node {
  return typeof x === "object" && x !== null && typeof (x as { type?: unknown }).type === "string";
}

function str(v: unknown, fallback = ""): string {
  return typeof v === "string" ? v : fallback;
}

// ---- Interaction + field context (one card = one scope) ----

interface EventEmit {
  emit: (action: string, data: unknown) => void;
  disabled: boolean;
  /** Read the card's live field values (keyed by field `name`). */
  fields: () => Record<string, unknown>;
  /** Write one field value. */
  setField: (name: string, value: unknown) => void;
}

const EventEmitContext = createContext<EventEmit>({
  emit: () => {},
  disabled: false,
  fields: () => ({}),
  setField: () => {},
});
const useEventEmit = () => useContext(EventEmitContext);

// ---- `rich`: the ONE text transform. `backtick` segments → mono spans. ----
// Still text nodes only — prose can never *choose* mono, it only *marks* machine
// tokens; the renderer decides the font.
function Rich(props: { text: unknown }): JSX.Element {
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
}

// ---- Node renderers (semantic tokens → DESIGN tokens) ----

function EyebrowNode(node: Node): JSX.Element {
  return (
    <div
      class={cn(
        "flex items-center gap-1.5 text-[0.62rem] font-semibold uppercase tracking-[0.06em]",
        eyebrowToneClass(str(node.tone)),
      )}
    >
      <Show when={node.boundary === true}>
        <span class="min-h-[13px] w-[3px] shrink-0 self-stretch rounded-sm bg-primary/40" aria-hidden="true" />
      </Show>
      <span>
        <Rich text={node.text} />
      </span>
    </div>
  );
}

function HeadingNode(node: Node): JSX.Element {
  // 16px ceiling: level-3 is a small sub-heading; default is the fork line.
  const cls =
    node.level === 3
      ? "text-[0.8rem] font-semibold leading-snug text-foreground"
      : "text-[0.95rem] font-semibold leading-snug tracking-[-0.005em] text-foreground";
  return (
    <div class={cls}>
      <Rich text={node.text} />
    </div>
  );
}

function TextNode(node: Node): JSX.Element {
  return (
    <p class={cn("text-[0.8rem] leading-relaxed text-foreground", toneTextClass(str(node.tone)))}>
      <Rich text={node.text} />
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
              <dt class="shrink-0 text-[0.62rem] font-semibold uppercase tracking-[0.04em] text-muted-foreground">
                <Rich text={item.label} />
              </dt>
              <dd class={cn("min-w-0 text-right text-xs text-foreground", toneTextClass(str(item.tone)))}>
                <Rich text={item.value} />
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
        "inline-flex w-fit items-center rounded-full px-2 py-0.5 text-[0.68rem] font-semibold",
        toneBadgeClass(str(node.tone)),
      )}
    >
      <Rich text={node.label} />
    </span>
  );
}

function StatusNode(node: Node): JSX.Element {
  const state = str(node.state, "neutral");
  return (
    <span class="inline-flex items-center gap-2 text-[0.8rem] text-foreground">
      <span
        class={cn(
          "size-1.5 shrink-0 rounded-full",
          statusDotClass(state),
          state === "running" ? "motion-safe:animate-pulse" : "",
        )}
        aria-hidden="true"
      />
      <span class={toneTextClass(str(node.tone))}>
        <Rich text={node.label} />
      </span>
    </span>
  );
}

/** One letter-keyed option. Recommended reads through exactly TWO cues — the
 * one-indigo row tint plus a small "Recommended" chip (§6); the key stays neutral
 * so the accent never triples up. A tap posts `choose {choice, label}`. */
function OptionListNode(node: Node): JSX.Element {
  const emit = useEventEmit();
  const action = str(node.action, "choose");
  const options = () => (Array.isArray(node.options) ? (node.options as Record<string, unknown>[]) : []);
  return (
    <div class="flex flex-col gap-1.5">
      <For each={options()}>
        {(opt) => {
          const recommended = opt.recommended === true;
          const key = str(opt.key);
          const label = str(opt.label);
          return (
            <button
              type="button"
              disabled={emit.disabled}
              class={cn(
                "grid w-full grid-cols-[24px_1fr_auto] items-start gap-2.5 rounded-md border p-2.5 text-left transition-colors",
                "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50 disabled:pointer-events-none disabled:opacity-60",
                recommended
                  ? "border-primary/40 bg-primary/[0.07]"
                  : "border-border bg-muted/40 hover:border-border/80",
              )}
              onClick={() => emit.emit(action, { choice: key, label: label.replace(/`/g, "") })}
            >
              <span class="grid size-6 shrink-0 place-items-center rounded border border-border bg-card text-[0.7rem] font-bold text-muted-foreground">
                {key}
              </span>
              <span class="flex min-w-0 flex-col">
                <span class="text-xs font-semibold leading-snug text-foreground">
                  <Rich text={opt.label} />
                </span>
                <Show when={str(opt.detail)}>
                  <span class="mt-0.5 text-[0.72rem] leading-snug text-muted-foreground">
                    <Rich text={opt.detail} />
                  </span>
                </Show>
              </span>
              <Show when={recommended}>
                <span class="mt-0.5 inline-flex items-center self-center rounded-full bg-primary/[0.12] px-2 py-0.5 text-[0.62rem] font-semibold text-primary">
                  Recommended
                </span>
              </Show>
            </button>
          );
        }}
      </For>
    </div>
  );
}

/** A text field, controlled by the card's shared field map (keyed by `name`).
 * A stray field outside any submit still renders (the renderer stays total). */
function FieldNode(node: Node): JSX.Element {
  const emit = useEventEmit();
  const name = str(node.name, "value");
  const label = str(node.label);
  const placeholder = str(node.placeholder);
  return (
    <div class="flex flex-col gap-1">
      <Show when={label}>
        <span class="text-[0.62rem] font-semibold uppercase tracking-[0.04em] text-muted-foreground">
          {label}
        </span>
      </Show>
      <input
        type="text"
        class="w-full rounded-md border border-border bg-background px-2.5 py-1.5 text-xs text-foreground outline-none placeholder:text-muted-foreground focus-visible:border-primary focus-visible:ring-2 focus-visible:ring-ring/40 disabled:opacity-60"
        placeholder={placeholder}
        aria-label={label || placeholder || name}
        disabled={emit.disabled}
        value={str(emit.fields()[name])}
        onInput={(e) => emit.setField(name, e.currentTarget.value)}
      />
    </div>
  );
}

/** Submit the card's collected field values back to the producer. `variant:
 * "ghost"` is the quiet secondary style; default is the one indigo. */
function SubmitNode(node: Node): JSX.Element {
  const emit = useEventEmit();
  const action = str(node.action, "submit");
  const ghost = node.variant === "ghost";
  return (
    <button
      type="button"
      disabled={emit.disabled}
      class={cn(
        "inline-flex h-8 w-fit select-none items-center gap-1.5 rounded-md px-3.5 text-xs font-semibold outline-none transition-colors",
        "focus-visible:ring-2 focus-visible:ring-ring/50 disabled:pointer-events-none disabled:opacity-60",
        ghost
          ? "border border-border bg-card text-muted-foreground hover:bg-muted"
          : "bg-primary text-primary-foreground hover:bg-primary/90",
      )}
      onClick={() => emit.emit(action, { ...emit.fields() })}
    >
      <Rich text={str(node.label, "Submit")} />
      <Show when={str(node.kbd)}>
        <span class="font-mono text-[0.62rem] opacity-80">{str(node.kbd)}</span>
      </Show>
    </button>
  );
}

/** The "accompanying dynamic UI" of a judgment: a mini diff or a small table.
 * A quiet inset framed by a hairline, labelled by its human `title` alone — the
 * internal `viewSlot` node-type token is never printed as copy (§6). */
function ViewSlotNode(node: Node): JSX.Element {
  const isDiff = node.variant === "diff";
  return (
    <div class="overflow-hidden rounded-md border border-border bg-muted/40">
      <div class="px-2.5 pb-1 pt-2 text-[0.62rem] font-bold uppercase tracking-[0.05em] text-muted-foreground">
        {str(node.title, "accompanying view")}
      </div>
      <Show
        when={isDiff}
        fallback={
          <div class="px-1 pb-1.5 pt-0.5">
            <For each={(Array.isArray(node.rows) ? node.rows : []) as Record<string, unknown>[]}>
              {(row) => (
                <div class="grid grid-cols-[auto_1fr_auto] items-center gap-2 border-b border-border/60 px-2 py-1.5 last:border-0">
                  <span class="flex items-center gap-1.5 text-xs font-medium text-foreground">
                    <Show when={str(row.state)}>
                      <span
                        class={cn(
                          "size-1.5 shrink-0 rounded-full",
                          statusDotClass(str(row.state)),
                          row.state === "running" ? "motion-safe:animate-pulse" : "",
                        )}
                        aria-hidden="true"
                      />
                    </Show>
                    {str(row.label)}
                  </span>
                  <span />
                  <span class="text-right text-[0.72rem] text-muted-foreground">
                    <Rich text={row.value} />
                  </span>
                </div>
              )}
            </For>
          </div>
        }
      >
        <div class="py-1.5 font-mono text-[0.68rem] leading-relaxed">
          <For each={(Array.isArray(node.lines) ? node.lines : []) as Record<string, unknown>[]}>
            {(ln) => {
              const op = str(ln.op);
              const prefix = op === "add" ? "+ " : op === "del" ? "- " : "  ";
              const cls =
                op === "add"
                  ? "bg-status-success/10 text-status-success"
                  : op === "del"
                    ? "bg-status-danger/10 text-status-danger"
                    : "text-muted-foreground";
              return <div class={cn("whitespace-pre px-2.5", cls)}>{prefix + str(ln.text)}</div>;
            }}
          </For>
        </div>
      </Show>
    </div>
  );
}

/** A grouping node with a top hairline (e.g. the record-rule field + submit). */
function InsetNode(node: Node): JSX.Element {
  return (
    <div class="mt-1 flex flex-col gap-2 border-t border-border pt-3">
      <NodeList nodes={childNodes(node)} />
    </div>
  );
}

function childNodes(node: Node): Node[] {
  return Array.isArray(node.children) ? node.children.filter(isNode) : [];
}

const REGISTRY: Record<string, (node: Node) => JSX.Element> = {
  eyebrow: EyebrowNode,
  heading: HeadingNode,
  text: TextNode,
  divider: DividerNode,
  keyValue: KeyValueNode,
  badge: BadgeNode,
  status: StatusNode,
  optionList: OptionListNode,
  field: FieldNode,
  submit: SubmitNode,
  viewSlot: ViewSlotNode,
  inset: InsetNode,
};

/** Unknown/unsupported node → a quiet dashed placeholder naming the type. Keeps
 * the surface honest ("something here we can't draw") without breaking the tree
 * or losing sibling content. */
function FallbackNode(props: { type: string }): JSX.Element {
  return (
    <div class="rounded-md border border-dashed border-border px-2.5 py-1.5 text-[0.72rem] text-muted-foreground">
      unsupported node: <span class="font-mono">{props.type || "(untyped)"}</span>
    </div>
  );
}

function EventNode(props: { node: unknown }): JSX.Element {
  return (
    <Show when={isNode(props.node)} fallback={null}>
      {(() => {
        const node = props.node as Node;
        const Comp = REGISTRY[node.type];
        return (
          <Show when={Comp} fallback={<FallbackNode type={node.type} />}>
            {Comp!(node)}
          </Show>
        );
      })()}
    </Show>
  );
}

function NodeList(props: { nodes: Node[] }): JSX.Element {
  return (
    <div class="flex flex-col gap-2.5">
      <For each={props.nodes}>{(n) => <EventNode node={n} />}</For>
    </div>
  );
}

export interface EventCardRendererProps {
  /** The constrained JSON-UI card body — a root node or an array of nodes. */
  ui: ViewSpec | ViewSpec[] | undefined;
  /** Post an interaction back (the caller turns it into an `event_action`). */
  onAction?: (action: string, data: unknown) => void;
  /** Disable interactive controls (e.g. the event is already decided). */
  disabled?: boolean;
}

/** Render one event card's `ui` tree. The whole tree is wrapped in an
 * ErrorBoundary so a malformed spec degrades to a quiet notice instead of
 * white-screening ("never throws"). */
export function EventCardRenderer(props: EventCardRendererProps): JSX.Element {
  const [values, setValues] = createSignal<Record<string, unknown>>({});
  const emit: EventEmit = {
    emit: (action, data) => props.onAction?.(action, data),
    get disabled() {
      return props.disabled === true;
    },
    fields: values,
    setField: (name, value) => setValues((prev) => ({ ...prev, [name]: value })),
  };
  return (
    <ErrorBoundary
      fallback={
        <div class="rounded-md border border-dashed border-border px-2.5 py-1.5 text-[0.72rem] text-muted-foreground">
          This card couldn't be displayed.
        </div>
      }
    >
      <EventEmitContext.Provider value={emit}>
        <NodeList nodes={eventUiNodes(props.ui)} />
      </EventEmitContext.Provider>
    </ErrorBoundary>
  );
}
