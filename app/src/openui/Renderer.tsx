/** The native Solid renderer for OpenUI Lang.
 *
 * A body is parsed, not executed: the streaming parser turns whatever has
 * arrived into a tree of known component names, drops the lines it cannot
 * understand, and this file draws the rest. A weak model can therefore produce
 * a usable interface from a partly wrong program, and an edited body re-renders
 * incrementally instead of from nothing. */
import { createMemo, createSignal, For, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createStreamingParser, type ElementNode, type ParseResult, type StreamParser } from "@openuidev/lang-core";
import { createOpenUiContext, type FormState, type OpenUiAction, type OpenUiContext, type OpenUiRenderer } from "./context";
import { hirselLibrary, type OpenUiLibrary } from "./library";

export interface RendererProps {
  /** OpenUI Lang v0.5 source. */
  body: string;
  library?: OpenUiLibrary;
  /** True while the body is still growing; controls stay inert until it settles. */
  streaming?: boolean;
  initialState?: FormState;
  onAction?: (action: OpenUiAction) => void;
}

/** Parse a body once, outside any render, for tests and for the warnings a
 * caller wants to show beside the drawing. */
export function parseOpenUi(body: string, library: OpenUiLibrary = hirselLibrary): ParseResult {
  return createStreamingParser(library.toJSONSchema()).set(body);
}

export function Renderer(props: RendererProps): JSX.Element {
  const library = () => props.library ?? hirselLibrary;
  // The parser is created once per library: `set()` diffs against its own
  // buffer, so re-feeding a growing body re-parses only the new statements.
  const parser = createMemo<StreamParser>(() => createStreamingParser(library().toJSONSchema()));
  const result = createMemo(() => parser().set(props.body ?? ""));
  const ctx = createOpenUiContext({
    initialState: props.initialState,
    streaming: () => props.streaming === true,
    onAction: action => props.onAction?.(action),
  });
  const tree = createMemo(() => renderValue(result().root, library(), ctx, undefined));
  return <div data-slot="openui-root" class="flex min-w-0 flex-col gap-4">
    {tree()}
    <ParseWarnings result={result()} />
  </div>;
}

/** What the parser could not use. Silent failure would leave the Owner
 * wondering whether the agent meant to omit a section. */
function ParseWarnings(props: { result: ParseResult }) {
  const [open, setOpen] = createSignal(false);
  const errors = () => props.result.meta.errors;
  const unresolved = () => props.result.meta.unresolved;
  const count = () => errors().length + unresolved().length;
  return <Show when={count() > 0}>
    <div data-slot="openui-warnings" class="flex min-w-0 flex-col gap-1 border-t border-border pt-2">
      <button type="button" class="min-h-11 self-start text-meta text-muted-foreground underline-offset-2 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        aria-expanded={open() ? "true" : "false"} onClick={() => setOpen(value => !value)}>
        {count()} {count() === 1 ? "line was" : "lines were"} dropped
      </button>
      <Show when={open()}>
        <ul class="flex flex-col gap-0.5">
          <For each={errors()}>{error => <li class="text-meta text-muted-foreground">{error.statementId ?? error.component}: {error.message}</li>}</For>
          <For each={unresolved()}>{name => <li class="text-meta text-muted-foreground">{name}: referenced but never defined</li>}</For>
        </ul>
      </Show>
    </div>
  </Show>;
}

/** Draw one parsed value. Strings and numbers are text, arrays are their
 * items in order, and an element is its library renderer — anything else,
 * including a component name this library does not define, draws nothing. */
function renderValue(value: unknown, library: OpenUiLibrary, ctx: OpenUiContext, formName: string | undefined): JSX.Element {
  if (value === null || value === undefined || value === false) return null;
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  if (Array.isArray(value)) return value.map(item => renderValue(item, library, ctx, formName));
  if (!isElementNode(value)) return null;
  const definition = library.components[value.typeName];
  if (!definition) return null;
  const render = definition.component as OpenUiRenderer<never>;
  try {
    return render({
      props: (value.props ?? {}) as never,
      renderNode: (nested, scope) => renderValue(nested, library, ctx, scope ?? formName),
      ctx,
      formName,
      statementId: value.statementId,
    });
  } catch (cause) {
    // One broken component must not take the rest of the artifact with it.
    return <p role="alert" class="text-meta text-status-danger">{value.typeName} could not be drawn. {String(cause instanceof Error ? cause.message : cause).slice(0, 200)}</p>;
  }
}

function isElementNode(value: unknown): value is ElementNode {
  return typeof value === "object" && value !== null && (value as { type?: unknown }).type === "element"
    && typeof (value as { typeName?: unknown }).typeName === "string";
}
