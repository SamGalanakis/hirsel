// The controls the two resident agents share: the provider select over the
// roster, the model control in whichever of its two shapes the chosen provider
// takes, and the prompt editor. Written once here so Main agent and Fork agent
// cannot drift apart — they are the same three questions asked of two slots.
import { LoaderCircle, Maximize2, SquarePen } from "lucide-solid";
import { createEffect, createSignal, onMount, Show, untrack } from "solid-js";
import { createFocusTrap } from "../../lib/focus";
import { createPendingKeys, type PendingKeys } from "../../lib/pending";
import type {
  AgentSlot,
  AvailableModel,
  ModelSelection,
  PromptDoc,
  ProviderInstance,
} from "../../protocol";
import { state } from "../../store/store";
import { getClient } from "../../ws/client";
import { Button } from "../ui/button";
import { Input } from "../ui/input";
import { PaneHeader } from "../ui/PaneHeader";
import { titleCase } from "./prefs";
import { Select } from "./rows";

/** Settle every pending control when a protocol `error` frame lands.
 *
 * The host validates first and RETURNS on failure — no change broadcast follows
 * a rejected write — so the error banner is the only settle signal a failed
 * command ever produces. Without this the control would sit disabled until its
 * timeout; with it, the surface recovers the moment the failure is visible.
 * (Timeout still backstops the silent cases: a no-op write the host declines to
 * echo, or a dropped frame.) */
export function settleOnProtocolError(pending: PendingKeys): void {
  let seen = untrack(() => state.protocolError);
  createEffect(() => {
    const error = state.protocolError;
    if (error === seen) return;
    seen = error;
    if (error !== null) pending.settleAll();
  });
}

/** The instances an agent may run on. Claude is available to Sub-agents only
 * (ADR-0015), so the host marks it `agent_selectable: false` and it never
 * appears in either agent's provider select. */
export function agentProviders(): ProviderInstance[] {
  return (state.providers?.instances ?? []).filter((instance) => instance.agent_selectable);
}

/** An instance's display label, falling back to its id when the roster does not
 * carry it (an older host, or a provider removed between frames). */
export function providerLabel(id: string | null | undefined): string {
  if (!id) return "";
  return state.providers?.instances.find((instance) => instance.id === id)?.label ?? id;
}

/** The provider one resident agent runs on. Changing it sends
 * `set_agent_provider` and settles when the agent's snapshot names the chosen
 * instance (or on an error frame, or on the timeout). */
export function AgentProviderRow(props: {
  slot: AgentSlot;
  /** The aria-name prefix — "Main agent" / "Fork agent". */
  name: string;
  providerId?: string;
  selectedProviderId?: string;
  pending: PendingKeys;
  onProviderChange?: (providerId: string) => void;
}) {
  const key = `${props.slot}-provider`;
  const [awaited, setAwaited] = createSignal<string | null>(null);

  createEffect(() => {
    const want = awaited();
    if (want !== null && props.providerId === want) {
      setAwaited(null);
      props.pending.settle(key);
    }
  });

  function change(id: string) {
    if (id === props.providerId) return;
    setAwaited(id);
    props.pending.begin(key);
    props.onProviderChange?.(id);
    getClient()?.setAgentProvider(props.slot, id);
  }

  return (
    <div class="flex items-center justify-between gap-3 py-3">
      <div class="flex min-w-0 items-center gap-2">
        <span class="text-sm text-foreground">Provider</span>
        <Show when={props.pending.isPending(key)}>
          <LoaderCircle
            class="size-3.5 shrink-0 animate-spin text-muted-foreground"
            aria-label="Saving"
          />
        </Show>
      </div>
      <Select
        ariaLabel={`${props.name} provider`}
        class="w-[10.5rem] shrink-0"
        disabled={props.pending.any()}
        value={props.selectedProviderId ?? props.providerId ?? ""}
        onChange={change}
        options={agentProviders().map((instance) => ({
          value: instance.id,
          label: instance.label,
        }))}
      />
    </div>
  );
}

export interface AgentModelView {
  current: ModelSelection;
  available: AvailableModel[];
  freeText: boolean;
  placeholder?: string;
}

/** Resolve model rows from the provider selected in the local dropdown. Older
 * hosts omit `selection`, in which case the stored snapshot remains the only
 * model metadata available and preserves the previous client behavior. */
export function agentModelView(
  slot: AgentSlot,
  selectedProviderId: string | undefined,
  storedProviderId: string | undefined,
  storedCurrent: ModelSelection,
  storedAvailable: AvailableModel[],
  storedFreeText: boolean,
): AgentModelView {
  const provider = state.providers?.instances.find(
    (instance) => instance.id === selectedProviderId,
  );
  const descriptor = provider?.selection;
  if (!descriptor) {
    return {
      current: storedCurrent,
      available: storedAvailable,
      freeText: storedFreeText,
      placeholder: provider?.default_model,
    };
  }
  if (descriptor.mode === "free_text") {
    return {
      current:
        selectedProviderId === storedProviderId
          ? storedCurrent
          : { id: provider.default_model ?? "", variant: "default" },
      available: [],
      freeText: true,
      placeholder: provider.default_model,
    };
  }

  const available = descriptor[slot];
  const model =
    available.find((candidate) => candidate.id === storedCurrent.id) ?? available.at(0);
  if (!model) {
    return {
      current: storedCurrent,
      available,
      freeText: false,
      placeholder: provider.default_model,
    };
  }
  return {
    current: {
      id: model.id,
      variant: model.variants.includes(storedCurrent.variant)
        ? storedCurrent.variant
        : model.default_variant,
    },
    available,
    freeText: false,
    placeholder: provider.default_model,
  };
}

/** The free-text shape of the model question: the provider takes any model id
 * it recognises, so the Owner types one. Its own draft, refused while blank —
 * an empty id is never sent, and the refusal is a quiet line, not an alarm. */
function FreeTextModelRow(props: {
  name: string;
  current: ModelSelection;
  placeholder?: string;
  pending: PendingKeys;
  pendingKey: string;
  onSave: (modelId: string) => void;
}) {
  const [draft, setDraft] = createSignal(props.current.id);
  const [refusal, setRefusal] = createSignal<string | null>(null);
  let settled = props.current.id;

  createEffect(() => {
    const next = props.current.id;
    if (next !== settled) {
      settled = next;
      setDraft(next);
    }
  });

  const busy = () => props.pending.any();

  function save() {
    const trimmed = draft().trim();
    if (trimmed.length === 0) {
      setRefusal("Enter a model id to save.");
      return;
    }
    setRefusal(null);
    props.pending.begin(props.pendingKey);
    props.onSave(trimmed);
  }

  return (
    <div class="py-3">
      <div class="flex flex-wrap items-center justify-between gap-3">
        <div class="flex min-w-0 items-center gap-2">
          <span class="text-sm text-foreground">Model</span>
          <Show when={busy()}>
            <LoaderCircle
              class="size-3.5 shrink-0 animate-spin text-muted-foreground"
              aria-label="Saving"
            />
          </Show>
        </div>
        <div class="flex shrink-0 items-center gap-2">
          <Input
            aria-label={`${props.name} model id`}
            class="h-9 w-[12rem] rounded-lg border border-border bg-surface px-2.5 font-mono text-xs text-foreground transition-colors hover:border-input focus-visible:ring-2 focus-visible:ring-ring"
            value={draft()}
            disabled={busy()}
            placeholder={props.placeholder}
            autocomplete="off"
            spellcheck={false}
            onInput={(event) => setDraft(event.currentTarget.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") save();
            }}
          />
          <Button
            size="sm"
            class="h-9"
            aria-label={`Save ${props.name} model id`}
            disabled={busy()}
            onClick={save}
          >
            Save
          </Button>
        </div>
      </div>
      <Show when={refusal()}>
        <p class="mt-1.5 text-xs leading-snug text-muted-foreground">{refusal()}</p>
      </Show>
    </div>
  );
}

/** The model question in whichever shape the chosen provider takes: a curated
 * model + reasoning pair, or one free-text model id. The provider decides;
 * there is no reasoning select in free-text mode because the provider chooses
 * it. */
export function AgentModelRows(props: {
  name: string;
  freeText: boolean;
  current: ModelSelection;
  available: AvailableModel[];
  /** The chosen provider's default model, shown as the free-text placeholder. */
  placeholder?: string;
  pending: PendingKeys;
  modelKey: string;
  variantKey: string;
  onSelect: (selection: ModelSelection) => void;
  onFreeText: (modelId: string) => void;
}) {
  const selectedModel = () => props.available.find((model) => model.id === props.current.id);
  const busy = () => props.pending.any();

  function changeModel(id: string) {
    if (id === props.current.id) return;
    const model = props.available.find((candidate) => candidate.id === id);
    // A model swap resets to that model's default variant (its `variant`s are a
    // different set); the variant control then reflects the settled truth.
    if (model) {
      props.pending.begin(props.modelKey);
      props.onSelect({ id, variant: model.default_variant });
    }
  }

  function changeVariant(variant: string) {
    if (variant === props.current.variant) return;
    props.pending.begin(props.variantKey);
    props.onSelect({ id: props.current.id, variant });
  }

  return (
    <Show
      when={!props.freeText && props.available.length > 0}
      fallback={
        <FreeTextModelRow
          name={props.name}
          current={props.current}
          placeholder={props.placeholder}
          pending={props.pending}
          pendingKey={props.modelKey}
          onSave={props.onFreeText}
        />
      }
    >
      <div class="flex items-center justify-between gap-3 py-3">
        <div class="flex min-w-0 items-center gap-2">
          <span class="text-sm text-foreground">Model</span>
          <Show when={props.pending.isPending(props.modelKey)}>
            <LoaderCircle
              class="size-3.5 shrink-0 animate-spin text-muted-foreground"
              aria-label="Saving"
            />
          </Show>
        </div>
        <Select
          ariaLabel={`${props.name} model`}
          class="w-[10.5rem] shrink-0"
          disabled={busy()}
          value={props.current.id}
          onChange={changeModel}
          options={props.available.map((model) => ({ value: model.id, label: model.label }))}
        />
      </div>
      <div class="flex items-center justify-between gap-3 py-3">
        <div class="flex min-w-0 items-center gap-2">
          <span class="text-sm text-foreground">Reasoning</span>
          <Show when={props.pending.isPending(props.variantKey)}>
            <LoaderCircle
              class="size-3.5 shrink-0 animate-spin text-muted-foreground"
              aria-label="Saving"
            />
          </Show>
        </div>
        <Select
          ariaLabel={`${props.name} reasoning variant`}
          class="w-[10.5rem] shrink-0"
          disabled={busy() || !selectedModel()}
          value={props.current.variant}
          onChange={changeVariant}
          options={(selectedModel()?.variants ?? []).map((variant) => ({
            value: variant,
            label: titleCase(variant),
          }))}
        />
      </div>
    </Show>
  );
}

/** The two actions a prompt draft has, written once so the inline row and the
 * expanded editor cannot drift apart. `Reset to default` clears the override
 * (one op, no pasting the bundled text back); on an already-default prompt with
 * local edits it just discards the draft, which needs no round trip. */
function PromptActions(props: {
  label: string;
  doc: () => PromptDoc;
  dirty: () => boolean;
  busy: () => boolean;
  onSave: () => void;
  onReset: () => void;
  class?: string;
}) {
  return (
    <div class={`flex items-center gap-2 ${props.class ?? ""}`}>
      <Button
        size="sm"
        class="h-9"
        aria-label={`Save ${props.label}`}
        disabled={props.busy() || !props.dirty()}
        onClick={props.onSave}
      >
        Save
      </Button>
      <Button
        size="sm"
        variant="ghost"
        class="h-9"
        aria-label={`Reset ${props.label} to default`}
        disabled={props.busy() || (props.doc().is_default && !props.dirty())}
        onClick={props.onReset}
      >
        Reset to default
      </Button>
      <Show when={props.doc().is_default}>
        <span class="text-xs text-muted-foreground">Bundled default</span>
      </Show>
    </div>
  );
}

/** The expanded prompt: the SAME draft, given the whole viewport.
 *
 * Deliberately the Settings modal's own pattern rather than a second one —
 * `fixed inset-0` over Settings, a focus trap whose Escape hands the Owner back
 * to the row they expanded from, and one PaneHeader with a ×. A prompt body is
 * the longest text in the product and a twelve-row box is not where it is
 * written; nothing about the draft changes on the way in or out, so expanding
 * mid-edit and collapsing again is free. */
function ExpandedPromptEditor(props: {
  label: string;
  doc: () => PromptDoc;
  draft: () => string;
  setDraft: (text: string) => void;
  dirty: () => boolean;
  busy: () => boolean;
  caption?: string;
  editorId: string;
  onSave: () => void;
  onReset: () => void;
  onClose: () => void;
  restoreTo: () => HTMLElement | null;
}) {
  let panelRef: HTMLDivElement | undefined;
  const titleId = `${props.editorId}-title`;

  onMount(() => {
    // Nested inside Settings' own trap: the stack hands Tab and Escape to this
    // one while it is open and gives them straight back on close, with focus
    // landing on the Expand control that summoned it.
    createFocusTrap(() => panelRef, {
      onEscape: props.onClose,
      restoreTo: props.restoreTo,
    });
  });

  return (
    <div
      ref={(node) => {
        panelRef = node;
      }}
      tabindex={-1}
      data-slot="prompt-editor-panel"
      role="dialog"
      aria-modal="true"
      aria-labelledby={titleId}
      /* Above Settings (`z-40`), the one surface it can ever open over. */
      class="fixed inset-0 z-50 flex flex-col bg-background outline-none pb-[env(safe-area-inset-bottom)]
        motion-safe:animate-in motion-safe:fade-in motion-safe:duration-200"
    >
      <PaneHeader
        icon={<SquarePen class="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />}
        title={props.label}
        titleId={titleId}
        onClose={props.onClose}
        closeLabel={`Close ${props.label}`}
        contentClass="mx-auto w-full max-w-frame px-gutter"
        badge={
          <Show when={props.busy()}>
            <LoaderCircle class="size-4 animate-spin text-muted-foreground" aria-label="Saving" />
          </Show>
        }
      />
      <div class="mx-auto flex min-h-0 w-full max-w-frame flex-1 flex-col px-gutter pt-4 pb-4">
        <textarea
          id={props.editorId}
          aria-label={`${props.label} (expanded)`}
          disabled={props.busy()}
          value={props.draft()}
          onInput={(event) => props.setDraft(event.currentTarget.value)}
          class="thin-scrollbar min-h-0 w-full flex-1 resize-none rounded-lg border border-border bg-surface px-3 py-2.5 font-mono text-xs leading-relaxed text-foreground outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-wait disabled:opacity-60"
        />
        <PromptActions
          class="mt-3"
          label={props.label}
          doc={props.doc}
          dirty={props.dirty}
          busy={props.busy}
          onSave={props.onSave}
          onReset={props.onReset}
        />
        <Show when={props.caption}>
          <p class="mt-2 text-xs leading-snug text-muted-foreground">{props.caption}</p>
        </Show>
      </div>
    </div>
  );
}

/** One editable prompt: a local draft until Save or Reset, then settled by the
 * authoritative `prompts_changed` frame. The inline row is the glance; Expand
 * gives the same draft the whole viewport. ONE state backs both, so a draft
 * survives every expand and collapse and a Save from either place settles the
 * same way. */
export function PromptEditor(props: {
  label: string;
  doc: () => PromptDoc;
  pending: PendingKeys;
  pendingKey: string;
  onSave: (text: string) => void;
  /** The one honest line about when this prompt takes effect, shown in the
   * expanded editor (the inline row already sits under its section's copy). */
  caption?: string;
  rows?: number;
}) {
  const [draft, setDraft] = createSignal(props.doc().text);
  const [expanded, setExpanded] = createSignal(false);
  let serverText = props.doc().text;

  // A changed server body replaces the draft. That is right for the Owner's own
  // accepted save, and blunt for a body that changed underneath them — another
  // device, or a hand edit of `hirsel.toml` — which discards whatever they were
  // typing. Pre-existing and left alone deliberately: the honest fix is a
  // conflict affordance, not a silently diverging local copy.
  createEffect(() => {
    const next = props.doc().text;
    if (next !== serverText) {
      serverText = next;
      setDraft(next);
    }
  });

  const dirty = () => draft() !== props.doc().text;
  const busy = () => props.pending.isPending(props.pendingKey);

  function save(text: string) {
    props.pending.begin(props.pendingKey);
    props.onSave(text);
  }

  function reset() {
    if (props.doc().is_default) {
      setDraft(props.doc().text);
      return;
    }
    save("");
  }

  const expandSlot = `prompt-expand-${props.pendingKey}`;

  return (
    <div class="py-3">
      {/* The inline row stands down entirely while the expanded editor is open:
          the overlay covers it anyway, and one editor at a time means one Save
          control, one label, and no duplicate ids for anything — assistive tech
          or test — to disambiguate. The draft lives out here, so standing the
          row down costs nothing. */}
      <Show
        when={!expanded()}
        fallback={
          <ExpandedPromptEditor
            label={props.label}
            doc={props.doc}
            draft={draft}
            setDraft={setDraft}
            dirty={dirty}
            busy={busy}
            caption={props.caption}
            editorId={`${props.pendingKey}-expanded-editor`}
            onSave={() => save(draft())}
            onReset={reset}
            onClose={() => setExpanded(false)}
            // Resolved by query AFTER the overlay unmounts, so it names the
            // Expand control that has just come back rather than the detached
            // node this component held while collapsed.
            restoreTo={() => document.querySelector<HTMLElement>(`[data-slot="${expandSlot}"]`)}
          />
        }
      >
        <div class="mb-2 flex items-center justify-between gap-3">
          <label class="text-sm text-foreground" for={`${props.pendingKey}-editor`}>
            {props.label}
          </label>
          <div class="flex shrink-0 items-center gap-2">
            <Show when={busy()}>
              <LoaderCircle
                class="size-3.5 animate-spin text-muted-foreground"
                aria-label="Saving"
              />
            </Show>
            <button
              type="button"
              data-slot={expandSlot}
              aria-label={`Expand ${props.label}`}
              onClick={() => setExpanded(true)}
              class="grid size-8 place-items-center rounded text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring [@media(pointer:coarse)]:size-11"
            >
              <Maximize2 class="size-4" aria-hidden="true" />
            </button>
          </div>
        </div>
        <textarea
          id={`${props.pendingKey}-editor`}
          aria-label={props.label}
          rows={props.rows ?? 12}
          disabled={busy()}
          value={draft()}
          onInput={(event) => setDraft(event.currentTarget.value)}
          class="thin-scrollbar w-full resize-y rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs leading-relaxed text-foreground outline-none transition-colors placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-wait disabled:opacity-60"
        />
        <PromptActions
          class="mt-2"
          label={props.label}
          doc={props.doc}
          dirty={dirty}
          busy={busy}
          onSave={() => save(draft())}
          onReset={reset}
        />
      </Show>
    </div>
  );
}

/** The one pending-key set an agent's controls share, already wired to settle
 * on a protocol error. Call inside the component that owns the subsection. */
export function createAgentPending(): PendingKeys {
  const pending = createPendingKeys();
  settleOnProtocolError(pending);
  return pending;
}
