// The interactive JSON-UI event-card renderer (ADR-0013) — the interactive
// sibling of ViewRenderer.tsx. It renders the CONSTRAINED event vocabulary
// (eyebrow · heading · text · keyValue · badge · status · divider · optionList ·
// field · submit · viewSlot · inset) onto DESIGN.md tokens, and degrades any
// unknown node to a quiet fallback chip — never throws, never blanks, never
// loses content. The nodes carry SEMANTIC tokens only (`tone`, `state`,
// `recommended`, `boundary`); the renderer alone owns the palette and the type
// scale, so a machine-authored instrument is on-brand by construction — it
// cannot express a raw color, a second accent, or a glow.
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
import {
  childNodes,
  createNodeDispatch,
  createSharedNodes,
  type Node,
  Notice,
  RichText as Rich,
  str,
  UnsupportedNode,
} from "./nodes";
import { eyebrowToneClass, statusDotClass } from "./tokens";

// The prop accessors, the `backtick` → mono text transform, the placeholder
// chip and the registry dispatch are shared with the generative-UI tier — see
// ./nodes.tsx. This tier is the denser instrument and lets a `status` label
// carry its tone.
const SHARED_NODES = createSharedNodes({
  Text: Rich,
  textLead: "max-w-[68ch]",
  keyValueValue: "text-xs",
  statusDot: "size-1.5",
  statusLabelTone: true,
});

// ---- Interaction + field context (one card = one scope) ----

interface EventEmit {
  emit: (action: string, data: unknown, settles: boolean) => void;
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

// ---- Card meta context (Wave-3 time axis) ----
// The blocking-judgment eyebrow ("Deciding unblocks N agents") gains the event's
// age ("· 6h"). The age lives on the EventItem, not in the `ui` tree, so it is
// threaded through a context the caller fills and the boundary eyebrow reads —
// keeping the renderer's inputs otherwise the pure `ui` vocabulary.
const CardMetaContext = createContext<{ eyebrowAge?: string }>({});
const useCardMeta = () => useContext(CardMetaContext);

// ---- Node renderers (semantic tokens → DESIGN tokens) ----

function EyebrowNode(node: Node): JSX.Element {
  const meta = useCardMeta();
  // The blocking-judgment boundary eyebrow gains the event age ("· 6h") — quiet,
  // tabular-nums, only when the caller supplies it (blocking judgments only).
  const age = () => (node.boundary === true ? meta.eyebrowAge : undefined);
  return (
    <div
      class={cn(
        "flex items-center gap-1.5 text-xs font-medium",
        eyebrowToneClass(str(node.tone)),
      )}
    >
      <span>
        <Rich text={node.text} />
        <Show when={age()}>
          <span class="tabular-nums text-muted-foreground"> · {age()}</span>
        </Show>
      </span>
    </div>
  );
}

function HeadingNode(node: Node): JSX.Element {
  // The Task identity immediately above the generated instrument is its h2.
  // The machine-authored question therefore owns h3 and the stronger type
  // scale; an explicitly nested heading steps down to h4 and stays quiet.
  if (node.level === 3) {
    return (
      <h4 class="text-sm font-medium leading-snug text-foreground">
        <Rich text={node.text} />
      </h4>
    );
  }
  return (
    <h3 class="max-w-[32ch] text-[clamp(1.75rem,3vw,2.25rem)] font-[500] leading-[1.12] tracking-[-0.025em] text-foreground">
      <Rich text={node.text} />
    </h3>
  );
}

/** One letter-keyed option. Choices are rows, not mini cards or capsules. */
function OptionListNode(node: Node): JSX.Element {
  const emit = useEventEmit();
  const action = str(node.action, "choose");
  const options = () => (Array.isArray(node.options) ? (node.options as Record<string, unknown>[]) : []);
  return (
    <div class="flex flex-col border-y border-border/70">
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
                "grid min-h-11 w-full grid-cols-[28px_1fr_auto] items-center gap-3 border-b border-border/70 px-1 py-2.5 text-left transition-colors last:border-b-0 active:translate-y-px",
                "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50 disabled:pointer-events-none disabled:opacity-60 disabled:active:translate-y-0",
                recommended
                  ? "bg-primary/[0.07]"
                  : "hover:bg-muted/40",
              )}
              onClick={() => emit.emit(
                action,
                { choice: key, label: label.replace(/`/g, "") },
                node.settles !== false,
              )}
            >
              <span class="font-mono text-xs font-semibold text-muted-foreground">
                {key}
              </span>
              <span class="flex min-w-0 flex-col">
                <span class="text-sm font-medium leading-snug text-foreground">
                  <Rich text={opt.label} />
                </span>
                <Show when={str(opt.detail)}>
                  <span class="mt-0.5 text-xs leading-snug text-muted-foreground">
                    <Rich text={opt.detail} />
                  </span>
                </Show>
              </span>
              <Show when={recommended}>
                <span class="self-center text-xs font-medium text-primary">
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
  const required = node.required === true;
  return (
    <div class="flex flex-col gap-1">
      <Show when={label}>
        <span class="text-xs font-medium text-muted-foreground">
          {label}
        </span>
      </Show>
      <input
        type="text"
        class="min-h-11 w-full rounded-lg border border-border bg-background px-3 py-2 text-sm text-foreground outline-none placeholder:text-muted-foreground focus-visible:border-primary focus-visible:ring-2 focus-visible:ring-ring/40 disabled:opacity-60"
        placeholder={placeholder}
        aria-label={label || placeholder || name}
        aria-required={required || undefined}
        required={required}
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
        "inline-flex min-h-11 w-fit select-none items-center gap-1.5 rounded-xl px-4 text-sm font-medium outline-none transition-colors active:translate-y-px",
        "focus-visible:ring-2 focus-visible:ring-ring/50 disabled:pointer-events-none disabled:opacity-60 disabled:active:translate-y-0",
        ghost
          ? "border border-border bg-card text-muted-foreground hover:bg-muted"
          : "bg-primary text-primary-foreground hover:bg-primary/90",
      )}
      onClick={() => emit.emit(action, { ...emit.fields() }, node.settles !== false)}
    >
      <Rich text={str(node.label, "Submit")} />
    </button>
  );
}

/** The "accompanying dynamic UI" of a judgment: a mini diff or a small table.
 * A quiet inset framed by a hairline, labelled by its human `title` alone — the
 * internal `viewSlot` node-type token is never printed as copy (§6). */
function ViewSlotNode(node: Node): JSX.Element {
  const isDiff = node.variant === "diff";
  return (
    <div class="overflow-hidden border-y border-border/70">
      <div class="px-1 pb-1 pt-2 text-xs font-medium text-muted-foreground">
        {str(node.title, "accompanying view")}
      </div>
      <Show
        when={isDiff}
        fallback={
          <div class="px-1 pb-1.5 pt-0.5">
            <For each={(Array.isArray(node.rows) ? node.rows : []) as Record<string, unknown>[]}>
              {(row) => (
                <div class="grid min-h-11 grid-cols-[auto_1fr_auto] items-center gap-2 border-b border-border/60 px-1 py-1.5 last:border-0">
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
                  <span class="flex items-center gap-2 text-right text-xs text-muted-foreground">
                    <Show when={str(row.state)}>
                      <span>{str(row.state)}</span>
                    </Show>
                    <Rich text={row.value} />
                  </span>
                </div>
              )}
            </For>
          </div>
        }
      >
        <div class="py-1.5 font-mono text-xs leading-relaxed">
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

const REGISTRY: Record<string, (node: Node) => JSX.Element> = {
  ...SHARED_NODES,
  eyebrow: EyebrowNode,
  heading: HeadingNode,
  optionList: OptionListNode,
  field: FieldNode,
  submit: SubmitNode,
  viewSlot: ViewSlotNode,
  inset: InsetNode,
};

/** Unknown/unsupported node → a quiet dashed placeholder naming the type. Keeps
 * the surface honest ("something here we can't draw") without breaking the tree
 * or losing sibling content. */
const EventNode = createNodeDispatch(REGISTRY, (type) => (
  <UnsupportedNode label="unsupported node:" type={type} />
));

/** Grouped vertical rhythm for the card body (craft wave). A uniform gap makes
 * the eyebrow, the question, the context and the choices read as one flat list;
 * grouped spacing lets the QUESTION cluster (eyebrow→heading tight) and sets the
 * CHOICES deliberately apart (context→options open). Everything else keeps the
 * calm default step. Returned as the top-margin for a node given its predecessor;
 * the first node gets none. */
function rhythmMargin(prevType: string | null, type: string): string {
  if (prevType === null) return "";
  // Eyebrow → heading: pull the question up under its own eyebrow.
  if (type === "heading" && prevType === "eyebrow") return "mt-1.5";
  // Anything → the choices: open the gap so the options sit apart from the ask.
  if (type === "optionList") return "mt-3";
  // The calm default step (the old uniform gap).
  return "mt-2.5";
}

/** The card body list. `rhythm` (the root card) groups the spacing per the rule
 * above; nested lists (an `inset`'s field+submit) keep their own tight uniform
 * gap so only the top-level question/choices rhythm is shaped. */
function NodeList(props: { nodes: Node[]; rhythm?: boolean }): JSX.Element {
  return (
    <Show
      when={props.rhythm}
      fallback={
        <div class="flex flex-col gap-2.5">
          <For each={props.nodes}>{(n) => <EventNode node={n} />}</For>
        </div>
      }
    >
      <div class="flex flex-col">
        <For each={props.nodes}>
          {(n, i) => (
            <div class={rhythmMargin(i() > 0 ? (props.nodes[i() - 1]?.type ?? null) : null, n.type)}>
              <EventNode node={n} />
            </div>
          )}
        </For>
      </div>
    </Show>
  );
}

export interface EventCardRendererProps {
  /** The constrained JSON-UI card body — a root node or an array of nodes. */
  ui: ViewSpec | ViewSpec[] | undefined;
  /** Post an interaction back (the caller turns it into an `event_action`). */
  onAction?: (action: string, data: unknown, settles: boolean) => void;
  /** Disable interactive controls (e.g. the event is already decided). */
  disabled?: boolean;
  /** Wave-3 time axis: the relative age appended to a blocking judgment's
   * boundary eyebrow ("Deciding unblocks 2 agents · 6h"). Omitted → no age. */
  eyebrowAge?: string;
}

/** Render one event card's `ui` tree. The whole tree is wrapped in an
 * ErrorBoundary so a malformed spec degrades to a quiet notice instead of
 * white-screening ("never throws"). */
export function EventCardRenderer(props: EventCardRendererProps): JSX.Element {
  const [values, setValues] = createSignal<Record<string, unknown>>({});
  const emit: EventEmit = {
    emit: (action, data, settles) => props.onAction?.(action, data, settles),
    get disabled() {
      return props.disabled === true;
    },
    fields: values,
    setField: (name, value) => setValues((prev) => ({ ...prev, [name]: value })),
  };
  return (
    <ErrorBoundary
      fallback={<Notice>This card couldn't be displayed.</Notice>}
    >
      <EventEmitContext.Provider value={emit}>
        <CardMetaContext.Provider value={{ get eyebrowAge() { return props.eyebrowAge; } }}>
          <NodeList nodes={eventUiNodes(props.ui)} rhythm />
        </CardMetaContext.Provider>
      </EventEmitContext.Provider>
    </ErrorBoundary>
  );
}
