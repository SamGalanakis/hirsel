// Generative-UI tier renderer. Renders a host-authored, fully-resolved view
// `spec` (a nested tree of the CLOSED catalog vocabulary — see
// templates/CATALOG.md) natively in the calm-terminal design system.
//
// Engine choice: hand-rolled recursive <ViewNode> over a component registry,
// NOT `@json-render/solid`. That library's `Spec` is a FLAT `{root, elements}`
// map with separated props/children-as-keys and an `on`/ActionBinding + state-
// store action model; hirsel's wire spec is a nested self-contained tree with
// implicit, type-driven events and NO client-side bindings/state (the host
// sends resolved concrete trees). Adapting would mean fighting the library's
// binding/state/action machinery for zero gain, so we own the ~18-type registry
// directly — which also gives full control over the "safe by vocabulary"
// guarantees: plain-text only (no HTML injection), unknown types degrade to a
// quiet placeholder, and every tone maps to a status token (see tokens.ts).
//
// The client NEVER resolves templates or bindings — `spec` is always concrete.
import { Check } from "lucide-solid";
import {
  createContext,
  createMemo,
  createSignal,
  ErrorBoundary,
  For,
  onCleanup,
  Show,
  useContext,
} from "solid-js";
import type { JSX } from "solid-js";
import type { ViewPlacement, ViewSpec } from "../protocol";
import { cn } from "@/lib/utils";
import { getClient } from "../ws/client";
import {
  calloutToneClass,
  PROGRESS_FILL,
  statusDotClass,
  toneBadgeClass,
  toneTextClass,
} from "./tokens";

/** How long an interactive control shows its pending/disabled state after a
 * submit. There is no direct ack — the reply returns through the normal
 * chat/ping flow (often replacing/clearing the view), so this is a bounded
 * fallback so a no-op never freezes the control permanently. */
const PENDING_MS = 4000;

// ---- Event context: interactive components emit through here ----

interface ViewEmit {
  instanceId: string;
  emit: (action: string, data: unknown) => void;
}

const ViewEmitContext = createContext<ViewEmit>({ instanceId: "", emit: () => {} });
const useViewEmit = () => useContext(ViewEmitContext);

/** A local "just submitted" flag that auto-clears after PENDING_MS (and on
 * unmount), used to disable + spin an interactive control briefly. */
function createSubmitting() {
  const [pending, setPending] = createSignal(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(timer));
  const begin = () => {
    setPending(true);
    clearTimeout(timer);
    timer = setTimeout(() => setPending(false), PENDING_MS);
  };
  return { pending, begin };
}

// ---- Safe prop accessors (nothing here may throw on malformed input) ----

/** A catalog component node: an object with a string `type`. */
type Node = ViewSpec;

function isNode(x: unknown): x is Node {
  return typeof x === "object" && x !== null && typeof (x as { type?: unknown }).type === "string";
}

/** Child component nodes of a container, skipping any non-node entries. */
function childNodes(node: Node): Node[] {
  const kids = node.children;
  return Array.isArray(kids) ? kids.filter(isNode) : [];
}

/** Render a catalog "display scalar" (string | number | boolean) as PLAIN text.
 * Never HTML — markdown/rich text is deferred to catalog v2, so this is the
 * "safe by vocabulary" boundary: agent-authored strings can never inject
 * markup. Nullish renders as empty. */
function scalarText(v: unknown): string {
  if (v === null || v === undefined) return "";
  if (typeof v === "boolean") return v ? "true" : "false";
  return String(v);
}

function str(v: unknown, fallback = ""): string {
  return typeof v === "string" ? v : fallback;
}

// ---- Layout token maps (complete literal classes for the JIT) ----

function gapClass(gap: unknown): string {
  switch (gap) {
    case "xs":
      return "gap-1";
    case "sm":
      return "gap-2";
    case "lg":
      return "gap-5";
    case "md":
    default:
      return "gap-3";
  }
}

function rowAlignClass(align: unknown): string {
  switch (align) {
    case "start":
      return "items-start";
    case "end":
      return "items-end";
    case "stretch":
      return "items-stretch";
    case "center":
    default:
      return "items-center";
  }
}

function headingClass(level: unknown): string {
  switch (level) {
    case 1:
      return "text-lg font-semibold tracking-[0.01em] text-foreground";
    case 3:
      return "text-sm font-semibold text-foreground";
    case 4:
      return "text-[0.72rem] font-semibold uppercase tracking-[0.06em] text-muted-foreground";
    case 2:
    default:
      return "text-base font-semibold text-foreground";
  }
}

function cellAlignClass(align: unknown): string {
  switch (align) {
    case "center":
      return "text-center";
    case "end":
      return "text-end";
    default:
      return "text-start";
  }
}

function buttonVariantClass(variant: unknown): string {
  switch (variant) {
    case "primary":
      return "bg-primary text-primary-foreground hover:bg-primary/90";
    case "danger":
      return "bg-status-danger/12 text-status-danger hover:bg-status-danger/20";
    case "secondary":
    default:
      return "border border-border bg-card text-foreground hover:bg-muted";
  }
}

const BTN_BASE =
  "inline-flex select-none items-center justify-center gap-1.5 rounded-md px-3 py-1.5 text-sm font-medium outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring/50 disabled:pointer-events-none disabled:opacity-60";

// ---- Catalog component renderers ----
// Each takes a node and returns JSX. They are plain functions invoked within
// the owner/context of <ViewNode>, so createSignal / useContext work.

function CardNode(node: Node): JSX.Element {
  const title = str(node.title);
  const subtitle = str(node.subtitle);
  return (
    <div class="flex flex-col gap-3 rounded-lg border border-border bg-card p-3.5">
      <Show when={title || subtitle}>
        <div class="flex flex-col gap-0.5">
          <Show when={title}>
            <div class="text-sm font-semibold text-foreground">{title}</div>
          </Show>
          <Show when={subtitle}>
            <div class="text-xs text-muted-foreground">{subtitle}</div>
          </Show>
        </div>
      </Show>
      <NodeList nodes={childNodes(node)} class="flex flex-col gap-3" />
    </div>
  );
}

function StackNode(node: Node): JSX.Element {
  return <NodeList nodes={childNodes(node)} class={cn("flex flex-col", gapClass(node.gap))} />;
}

function RowNode(node: Node): JSX.Element {
  return (
    <NodeList
      nodes={childNodes(node)}
      class={cn(
        "flex flex-row",
        gapClass(node.gap),
        rowAlignClass(node.align),
        node.wrap ? "flex-wrap" : "min-w-0",
      )}
    />
  );
}

function HeadingNode(node: Node): JSX.Element {
  return <div class={headingClass(node.level)}>{scalarText(node.text)}</div>;
}

function TextNode(node: Node): JSX.Element {
  return (
    <p class={cn("whitespace-pre-wrap text-sm leading-relaxed text-foreground", toneTextClass(str(node.tone)))}>
      {scalarText(node.text)}
    </p>
  );
}

function DividerNode(): JSX.Element {
  return <hr class="border-t border-border" />;
}

function KeyValueNode(node: Node): JSX.Element {
  const items = Array.isArray(node.items) ? node.items : [];
  return (
    <dl class="flex flex-col gap-1.5">
      <For each={items}>
        {(raw) => {
          const item = (raw ?? {}) as Record<string, unknown>;
          return (
            <div class="flex items-baseline justify-between gap-3">
              <dt class="shrink-0 text-xs text-muted-foreground">{scalarText(item.label)}</dt>
              <dd
                class={cn(
                  "min-w-0 text-right text-sm text-foreground",
                  toneTextClass(str(item.tone)),
                )}
              >
                {scalarText(item.value)}
              </dd>
            </div>
          );
        }}
      </For>
    </dl>
  );
}

function TableNode(node: Node): JSX.Element {
  const columns = (Array.isArray(node.columns) ? node.columns : []) as Record<string, unknown>[];
  const rows = (Array.isArray(node.rows) ? node.rows : []) as Record<string, unknown>[];
  const caption = str(node.caption);
  return (
    <div class="overflow-x-auto">
      <table class="w-full border-collapse text-sm">
        <Show when={caption}>
          <caption class="mb-1.5 text-left text-xs text-muted-foreground">{caption}</caption>
        </Show>
        <thead>
          <tr class="border-b border-border">
            <For each={columns}>
              {(col) => (
                <th
                  class={cn(
                    "px-2 py-1.5 text-xs font-medium text-muted-foreground",
                    cellAlignClass(col.align),
                  )}
                >
                  {scalarText(col.label)}
                </th>
              )}
            </For>
          </tr>
        </thead>
        <tbody>
          <For each={rows}>
            {(row) => (
              <tr class="border-b border-border/60 last:border-0">
                <For each={columns}>
                  {(col) => (
                    <td class={cn("px-2 py-1.5 text-foreground", cellAlignClass(col.align))}>
                      {scalarText((row ?? {})[str(col.key)])}
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

function ListNode(node: Node): JSX.Element {
  const items = Array.isArray(node.items) ? node.items : [];
  const ordered = node.ordered === true;
  const body = (
    <For each={items}>
      {(raw) => {
        const item = (raw ?? {}) as Record<string, unknown>;
        return (
          <li class={cn("text-sm leading-relaxed text-foreground", toneTextClass(str(item.tone)))}>
            {scalarText(item.text)}
          </li>
        );
      }}
    </For>
  );
  return (
    <Show
      when={ordered}
      fallback={<ul class="ml-4 flex list-disc flex-col gap-1 marker:text-muted-foreground">{body}</ul>}
    >
      <ol class="ml-4 flex list-decimal flex-col gap-1 marker:text-muted-foreground">{body}</ol>
    </Show>
  );
}

function ChecklistNode(node: Node): JSX.Element {
  const items = Array.isArray(node.items) ? node.items : [];
  return (
    <ul class="flex flex-col gap-1.5">
      <For each={items}>
        {(raw) => {
          const item = (raw ?? {}) as Record<string, unknown>;
          const checked = item.checked === true;
          return (
            <li class="flex items-start gap-2">
              <span
                class={cn(
                  "mt-0.5 grid size-4 shrink-0 place-items-center rounded border",
                  checked
                    ? "border-status-success bg-status-success/15 text-status-success"
                    : "border-border text-transparent",
                )}
                aria-hidden="true"
              >
                <Check class="size-3" />
              </span>
              <span class="flex min-w-0 flex-col">
                <span
                  class={cn(
                    "text-sm leading-snug",
                    checked ? "text-muted-foreground line-through" : "text-foreground",
                  )}
                >
                  {scalarText(item.label)}
                </span>
                <Show when={str(item.detail)}>
                  <span class="text-xs text-muted-foreground">{str(item.detail)}</span>
                </Show>
              </span>
            </li>
          );
        }}
      </For>
    </ul>
  );
}

function BadgeNode(node: Node): JSX.Element {
  return (
    <span
      class={cn(
        "inline-flex w-fit items-center rounded-full px-2 py-0.5 text-xs font-medium",
        toneBadgeClass(str(node.tone)),
      )}
    >
      {scalarText(node.label)}
    </span>
  );
}

function StatusNode(node: Node): JSX.Element {
  const state = str(node.state, "neutral");
  return (
    <span class="inline-flex items-center gap-1.5 text-sm text-foreground">
      <span
        class={cn(
          "size-2 shrink-0 rounded-full",
          statusDotClass(state),
          state === "running" ? "motion-safe:animate-pulse" : "",
        )}
        aria-hidden="true"
      />
      {scalarText(node.label)}
    </span>
  );
}

function ProgressNode(node: Node): JSX.Element {
  const raw = typeof node.value === "number" && Number.isFinite(node.value) ? node.value : 0;
  const value = Math.max(0, Math.min(1, raw));
  const pct = Math.round(value * 100);
  const label = str(node.label);
  return (
    <div class="flex flex-col gap-1">
      <Show when={label}>
        <div class="flex items-baseline justify-between gap-2">
          <span class="text-xs text-muted-foreground">{label}</span>
          <span class="font-mono text-xs tabular-nums text-muted-foreground">{pct}%</span>
        </div>
      </Show>
      <div
        class="h-1.5 w-full overflow-hidden rounded-full bg-muted"
        role="progressbar"
        aria-valuenow={pct}
        aria-valuemin={0}
        aria-valuemax={100}
      >
        <div class={cn("h-full rounded-full transition-[width]", PROGRESS_FILL)} style={{ width: `${pct}%` }} />
      </div>
    </div>
  );
}

function CalloutNode(node: Node): JSX.Element {
  const title = str(node.title);
  return (
    <div class={cn("flex flex-col gap-1 rounded-md border-l-2 px-3 py-2.5", calloutToneClass(str(node.tone)))}>
      <Show when={title}>
        <div class="text-sm font-semibold text-foreground">{title}</div>
      </Show>
      <div class="whitespace-pre-wrap text-sm leading-relaxed text-foreground">{scalarText(node.body)}</div>
    </div>
  );
}

function ActionNode(node: Node): JSX.Element {
  const { emit } = useViewEmit();
  const { pending, begin } = createSubmitting();
  const action = str(node.action);
  const data = "data" in node ? node.data : null;
  return (
    <button
      type="button"
      class={cn(BTN_BASE, buttonVariantClass(node.variant), "w-fit")}
      disabled={pending()}
      onClick={() => {
        begin();
        emit(action, data ?? null);
      }}
    >
      <Show when={pending()}>
        <span class="size-3.5 animate-spin rounded-full border-2 border-current border-t-transparent" aria-hidden="true" />
      </Show>
      {scalarText(node.label)}
    </button>
  );
}

function OptionSetNode(node: Node): JSX.Element {
  const { emit } = useViewEmit();
  const { pending, begin } = createSubmitting();
  const action = str(node.action);
  const choices = (Array.isArray(node.choices) ? node.choices : []) as Record<string, unknown>[];
  const label = str(node.label);
  return (
    <div class="flex flex-col gap-1.5">
      <Show when={label}>
        <span class="text-xs font-medium text-muted-foreground">{label}</span>
      </Show>
      <div class="flex flex-wrap gap-1.5">
        <For each={choices}>
          {(choice) => {
            const selected = "selected" in node && node.selected === choice.value;
            return (
              <button
                type="button"
                class={cn(
                  BTN_BASE,
                  "flex-col items-start gap-0",
                  selected
                    ? "bg-primary text-primary-foreground hover:bg-primary/90"
                    : "border border-border bg-card text-foreground hover:bg-muted",
                )}
                disabled={pending()}
                aria-pressed={selected}
                onClick={() => {
                  begin();
                  emit(action, { value: choice.value });
                }}
              >
                <span>{scalarText(choice.label)}</span>
                <Show when={str(choice.description)}>
                  <span
                    class={cn(
                      "text-xs font-normal",
                      selected ? "text-primary-foreground/80" : "text-muted-foreground",
                    )}
                  >
                    {str(choice.description)}
                  </span>
                </Show>
              </button>
            );
          }}
        </For>
      </div>
    </div>
  );
}

/** One form field, controlled by the parent <FormNode>. `value` writes back
 * into the form's value map keyed by the field `name`. */
function FormField(props: {
  field: Record<string, unknown>;
  value: unknown;
  disabled: boolean;
  onChange: (name: string, value: unknown) => void;
}): JSX.Element {
  const name = str(props.field.name);
  const label = str(props.field.label);
  const kind = str(props.field.kind, "text");
  const placeholder = str(props.field.placeholder);
  const required = props.field.required === true;
  const controlBase =
    "w-full rounded-md border border-border bg-background px-2.5 py-1.5 text-sm text-foreground outline-none placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring/50 disabled:opacity-60";

  return (
    <label class="flex flex-col gap-1">
      <span class="flex items-center gap-1 text-xs font-medium text-muted-foreground">
        {label}
        <Show when={required}>
          <span class="text-status-danger" aria-hidden="true">
            *
          </span>
        </Show>
      </span>
      <Show when={kind === "textarea"}>
        <textarea
          class={cn(controlBase, "min-h-16 resize-y")}
          placeholder={placeholder}
          required={required}
          disabled={props.disabled}
          value={str(props.value)}
          onInput={(e) => props.onChange(name, e.currentTarget.value)}
        />
      </Show>
      <Show when={kind === "text"}>
        <input
          type="text"
          class={controlBase}
          placeholder={placeholder}
          required={required}
          disabled={props.disabled}
          value={str(props.value)}
          onInput={(e) => props.onChange(name, e.currentTarget.value)}
        />
      </Show>
      <Show when={kind === "number"}>
        <input
          type="number"
          class={controlBase}
          placeholder={placeholder}
          required={required}
          disabled={props.disabled}
          value={props.value === null || props.value === undefined ? "" : String(props.value)}
          onInput={(e) => {
            const v = e.currentTarget.value;
            props.onChange(name, v === "" ? null : Number(v));
          }}
        />
      </Show>
      <Show when={kind === "toggle"}>
        <span class="inline-flex items-center gap-2 py-0.5">
          <input
            type="checkbox"
            class="size-4 accent-[var(--primary)] disabled:opacity-60"
            required={required}
            disabled={props.disabled}
            checked={props.value === true}
            onChange={(e) => props.onChange(name, e.currentTarget.checked)}
          />
          <span class="text-sm text-muted-foreground">{placeholder || "Enabled"}</span>
        </span>
      </Show>
      <Show when={kind === "select"}>
        <select
          class={controlBase}
          required={required}
          disabled={props.disabled}
          value={str(props.value)}
          onChange={(e) => props.onChange(name, e.currentTarget.value)}
        >
          <For each={(Array.isArray(props.field.options) ? props.field.options : []) as Record<string, unknown>[]}>
            {(opt) => <option value={scalarText(opt.value)}>{scalarText(opt.label)}</option>}
          </For>
        </select>
      </Show>
    </label>
  );
}

function FormNode(node: Node): JSX.Element {
  const { emit } = useViewEmit();
  const { pending, begin } = createSubmitting();
  const action = str(node.action);
  const fields = (Array.isArray(node.fields) ? node.fields : []).filter(isNode);
  const submitLabel = str(node.submitLabel, "Submit");

  // Seed the value map from each field's declared `value` (nullish → empty).
  const seed: Record<string, unknown> = {};
  for (const f of fields) {
    const name = str((f as Record<string, unknown>).name);
    if (!name) continue;
    const v = (f as Record<string, unknown>).value;
    const kind = str((f as Record<string, unknown>).kind, "text");
    seed[name] = v ?? (kind === "toggle" ? false : kind === "number" ? null : "");
  }
  const [values, setValues] = createSignal<Record<string, unknown>>(seed);
  const onChange = (name: string, value: unknown) =>
    setValues((prev) => ({ ...prev, [name]: value }));

  return (
    <form
      class="flex flex-col gap-3"
      onSubmit={(e) => {
        e.preventDefault();
        begin();
        emit(action, values());
      }}
    >
      <For each={fields}>
        {(field) => {
          const rec = field as Record<string, unknown>;
          const name = str(rec.name);
          return (
            <FormField
              field={rec}
              value={values()[name]}
              disabled={pending()}
              onChange={onChange}
            />
          );
        }}
      </For>
      <button
        type="submit"
        class={cn(BTN_BASE, buttonVariantClass("primary"), "w-fit")}
        disabled={pending()}
      >
        <Show when={pending()}>
          <span class="size-3.5 animate-spin rounded-full border-2 border-current border-t-transparent" aria-hidden="true" />
        </Show>
        {submitLabel}
      </button>
    </form>
  );
}

/** A stray `field` outside a form (the catalog only allows fields inside forms,
 * but the renderer stays total): render it read-only, no submission wiring. */
function FieldNode(node: Node): JSX.Element {
  return (
    <FormField field={node as Record<string, unknown>} value={(node as Record<string, unknown>).value} disabled onChange={() => {}} />
  );
}

const REGISTRY: Record<string, (node: Node) => JSX.Element> = {
  card: CardNode,
  stack: StackNode,
  row: RowNode,
  heading: HeadingNode,
  text: TextNode,
  divider: DividerNode,
  keyValue: KeyValueNode,
  table: TableNode,
  list: ListNode,
  checklist: ChecklistNode,
  badge: BadgeNode,
  status: StatusNode,
  progress: ProgressNode,
  callout: CalloutNode,
  action: ActionNode,
  optionSet: OptionSetNode,
  field: FieldNode,
  form: FormNode,
};

/** Fallback for an unknown/unsupported catalog type: a quiet placeholder rather
 * than a thrown error or a blank. Keeps the surface honest ("something here we
 * can't draw") without breaking the rest of the tree. */
function UnknownNode(props: { type: string }): JSX.Element {
  return (
    <div class="rounded-md border border-dashed border-border px-2.5 py-1.5 text-xs text-muted-foreground">
      Unsupported view component: <span class="font-mono">{props.type || "(untyped)"}</span>
    </div>
  );
}

/** Recursively render one node via the registry. Non-node values render
 * nothing; unknown types fall back to the placeholder. */
function ViewNode(props: { node: unknown }): JSX.Element {
  return (
    <Show when={isNode(props.node)} fallback={null}>
      {(() => {
        const node = props.node as Node;
        const Comp = REGISTRY[node.type];
        return <Show when={Comp} fallback={<UnknownNode type={node.type} />}>{Comp!(node)}</Show>;
      })()}
    </Show>
  );
}

/** Render a list of nodes inside a container with the given class. */
function NodeList(props: { nodes: Node[]; class?: string }): JSX.Element {
  return (
    <div class={props.class}>
      <For each={props.nodes}>{(n) => <ViewNode node={n} />}</For>
    </div>
  );
}

export interface ViewRendererProps {
  /** The resolved, concrete catalog tree (host-authored). */
  spec: ViewSpec;
  /** The owning instance — every emitted `view_event` carries it. */
  instanceId: string;
  /** Where this view is surfaced (canvas | chat | ping:<id>). Carried for
   * context/telemetry; the visual output is placement-independent. */
  placement: ViewPlacement;
  /** Test/host seam for owner-initiated events. Defaults to the ws client's
   * `sendViewEvent`. */
  onEvent?: (event: { instanceId: string; action: string; data: unknown }) => void;
}

/** Public entry point. Renders a view spec, providing the emit context so
 * interactive components (`action` / `optionSet` / `form`) can send
 * `view_event` frames. The whole tree is wrapped in an ErrorBoundary so a
 * malformed spec degrades to a quiet notice instead of white-screening the app
 * ("never throws"). */
export function ViewRenderer(props: ViewRendererProps): JSX.Element {
  const emit = (action: string, data: unknown) => {
    if (props.onEvent) {
      props.onEvent({ instanceId: props.instanceId, action, data });
      return;
    }
    getClient()?.sendViewEvent(props.instanceId, action, data);
  };
  // "Update in place is just a re-view_upsert of the same id" (wire contract).
  // The store `reconcile`s views keyed by instance_id, deep-mutating a view's
  // `spec` IN PLACE (reference preserved), so keying re-render on identity would
  // miss it. A serialized key deep-reads every leaf (subscribing to all of
  // them) and, on any change, recreates the whole view subtree — so an updated
  // spec always re-renders regardless of how individual components read props.
  // Views are small and updates infrequent (calm surface), so this is cheap.
  const specKey = createMemo(() => {
    try {
      return JSON.stringify(props.spec);
    } catch {
      return String(props.instanceId);
    }
  });
  return (
    <ErrorBoundary
      fallback={
        <div class="rounded-md border border-dashed border-border px-2.5 py-1.5 text-xs text-muted-foreground">
          This view couldn't be displayed.
        </div>
      }
    >
      <ViewEmitContext.Provider value={{ instanceId: props.instanceId, emit }}>
        <div data-slot="view" data-placement={props.placement}>
          <Show when={specKey()} keyed>
            {(_key: string) => <ViewNode node={props.spec} />}
          </Show>
        </div>
      </ViewEmitContext.Provider>
    </ErrorBoundary>
  );
}
