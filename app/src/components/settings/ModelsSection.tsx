// Settings → Models: the canonical home for main-agent model selection and the
// sub-agent model catalog. Both subsections are host-backed — they reflect the
// store snapshots the Host broadcasts and send commands over the WS client.
import { ChevronDown, LoaderCircle } from "lucide-solid";
import { createEffect, createSignal, For, type JSX, Show, untrack } from "solid-js";
import { createPendingKeys, type PendingKeys } from "../../lib/pending";
import type { ModelSelection, SubagentModel } from "../../protocol";
import { state } from "../../store/store";
import { getClient } from "../../ws/client";
import { titleCase } from "./prefs";
import { Group, SectionHeader, SubHeading, Select, Toggle } from "./rows";

/** Settle every pending control when a protocol `error` frame lands.
 *
 * The host validates first and RETURNS on failure — no `model_changed` /
 * `subagent_models_changed` broadcast follows a rejected write — so the error
 * banner is the only settle signal a failed command ever produces. Without this
 * the control would sit disabled until its timeout; with it, the surface
 * recovers the moment the failure is visible. (Timeout still backstops the
 * silent cases: a no-op write the host declines to echo, or a dropped frame.) */
function settleOnProtocolError(pending: PendingKeys): void {
  let seen = untrack(() => state.protocolError);
  createEffect(() => {
    const error = state.protocolError;
    if (error === seen) return;
    seen = error;
    if (error !== null) pending.settleAll();
  });
}

/** Main-agent model + reasoning-variant controls, reflecting `state.model`.
 * Changing either sends `set_model`; the tapped control shows a brief pending
 * spinner and the `model_changed` broadcast settles the store. Hidden when the
 * host reports no model snapshot (older hosts). */
function MainAgentModel() {
  const current = () => state.model?.current;
  const available = () => state.model?.available ?? [];
  const selectedModel = () => available().find((m) => m.id === current()?.id);

  // Which control is awaiting a settle ("model" / "variant"), so the spinner
  // sits on the changed control only. Bounded: a matching broadcast, an error
  // frame, or the timeout — the value we're awaiting is NOT guaranteed to be
  // echoed (the host only broadcasts an actual change).
  const pending = createPendingKeys();
  const [awaited, setAwaited] = createSignal<ModelSelection | null>(null);
  createEffect(() => {
    const sel = awaited();
    const c = current();
    if (sel && c && c.id === sel.id && c.variant === sel.variant) {
      setAwaited(null);
      pending.settleAll();
    }
  });
  settleOnProtocolError(pending);

  function send(sel: ModelSelection, control: "model" | "variant") {
    setAwaited(sel);
    pending.begin(control);
    getClient()?.setModel(sel.id, sel.variant);
  }

  function onModelChange(id: string) {
    if (id === current()?.id) return;
    const m = available().find((a) => a.id === id);
    if (!m) return;
    // A model swap resets to that model's default variant (its `variant`s are a
    // different set); the variant control then reflects the settled truth.
    send({ id, variant: m.default_variant }, "model");
  }

  function onVariantChange(variant: string) {
    const id = current()?.id;
    if (!id || variant === current()?.variant) return;
    send({ id, variant }, "variant");
  }

  const busy = () => pending.any();

  return (
    <Show when={current()}>
      <SubHeading>Main agent</SubHeading>
      <Group class="divide-y divide-border">
        <div class="flex items-center justify-between gap-3 py-3">
          <div class="flex min-w-0 items-center gap-2">
            <span class="text-sm text-foreground">Model</span>
            <Show when={pending.isPending("model")}>
              <LoaderCircle class="size-3.5 shrink-0 animate-spin text-muted-foreground" aria-label="Saving" />
            </Show>
          </div>
          <Select
            ariaLabel="Main agent model"
            class="w-[10.5rem] shrink-0"
            disabled={busy()}
            value={current()?.id ?? ""}
            onChange={onModelChange}
            options={available().map((m) => ({ value: m.id, label: m.label }))}
          />
        </div>
        <div class="flex items-center justify-between gap-3 py-3">
          <div class="flex min-w-0 items-center gap-2">
            <span class="text-sm text-foreground">Reasoning</span>
            <Show when={pending.isPending("variant")}>
              <LoaderCircle class="size-3.5 shrink-0 animate-spin text-muted-foreground" aria-label="Saving" />
            </Show>
          </div>
          <Select
            ariaLabel="Main agent reasoning variant"
            class="w-[10.5rem] shrink-0"
            disabled={busy() || !selectedModel()}
            value={current()?.variant ?? ""}
            onChange={onVariantChange}
            options={(selectedModel()?.variants ?? []).map((v) => ({ value: v, label: titleCase(v) }))}
          />
        </div>
      </Group>
    </Show>
  );
}

/** One sub-agent model row: identity + master toggle + a quiet multi-select
 * variant field. Any change sends the FULL row state via
 * `set_subagent_model`; `pending` (shared across the section) fades the row
 * until the `subagent_models_changed` broadcast settles the catalog. */
function SubagentModelRow(props: {
  provider: string;
  model: SubagentModel;
  pending: boolean;
  onChange: (patch: { enabled?: boolean; enabled_variants?: string[] }) => void;
}) {
  const selected = (variant: string) => props.model.enabled_variants.includes(variant);

  function toggleVariant(variant: string) {
    const enabledVariants = selected(variant)
      ? props.model.enabled_variants.filter((enabled) => enabled !== variant)
      : props.model.variants.filter(
          (candidate) =>
            candidate === variant || props.model.enabled_variants.includes(candidate),
        );
    if (enabledVariants.length === 0) return;
    props.onChange({ enabled_variants: enabledVariants });
  }

  return (
    <div
      class="py-3 transition-opacity"
      classList={{ "opacity-60": props.pending }}
    >
      <div class="flex items-center gap-3">
        <div class="min-w-0 flex-1">
          <div class="truncate text-sm text-foreground">{props.model.label}</div>
          <div class="mt-0.5 truncate font-mono text-meta text-muted-foreground">
            {props.model.id}
          </div>
        </div>
        <Toggle
          ariaLabel={`Enable ${props.model.label}`}
          checked={props.model.enabled}
          disabled={props.pending}
          onChange={(enabled) => props.onChange({ enabled })}
        />
      </div>
      <div
        role="group"
        aria-label={`${props.model.label} enabled variants`}
        aria-disabled={!props.model.enabled}
        class="mt-3 flex flex-wrap gap-1.5 transition-opacity"
        classList={{ "opacity-45": !props.model.enabled }}
      >
        <For each={props.model.variants}>
          {(variant) => {
            const active = () => selected(variant);
            const isLast = () => active() && props.model.enabled_variants.length === 1;
            return (
              <button
                type="button"
                aria-pressed={active()}
                aria-label={`${active() ? "Disable" : "Enable"} ${props.model.label} ${variant} variant`}
                disabled={props.pending || !props.model.enabled || isLast()}
                title={isLast() ? "At least one variant must stay enabled" : undefined}
                onClick={() => toggleVariant(variant)}
                class="min-h-8 rounded-full border px-2.5 text-xs font-medium outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-55 [@media(pointer:coarse)]:min-h-11 [@media(pointer:coarse)]:px-3.5"
                classList={{
                  "border-primary/60 bg-primary/10 text-foreground": active(),
                  "border-border bg-surface text-muted-foreground hover:border-input hover:text-foreground":
                    !active(),
                }}
              >
                {titleCase(variant)}
              </button>
            );
          }}
        </For>
      </div>
    </div>
  );
}

/** Sub-agent model catalog, grouped by provider. Hidden when the host reports
 * no catalog (older hosts). */
function SubagentModels() {
  const catalog = () => state.subagentModels;
  const [collapsedProviders, setCollapsedProviders] = createSignal<Set<string>>(new Set());
  const catalogVersion = () =>
    catalog()
      ?.providers.flatMap((provider) =>
        provider.models.map(
          (model) =>
            `${provider.provider}:${model.id}:${model.enabled}:${model.enabled_variants.join(",")}`,
        ),
      )
      .join("|") ?? "";

  // Rows awaiting the broadcast, keyed `provider\u0000modelId`. Cleared whenever
  // the catalog content changes (a broadcast settled the truth); Solid's store
  // updates nested catalog objects in place, so tracking the root reference is
  // insufficient. Coarse but correct: the broadcast is authoritative for all.
  // A rejected or no-op write produces no broadcast at all, so each key is also
  // bounded by the error frame and by its timeout.
  const pending = createPendingKeys();
  createEffect(() => {
    catalogVersion();
    pending.settleAll();
  });
  settleOnProtocolError(pending);

  const keyOf = (provider: string, id: string) => `${provider}\u0000${id}`;
  const providerPanelId = (provider: string) => `subagent-provider-${provider}`;
  const isCollapsed = (provider: string) => collapsedProviders().has(provider);

  function toggleProvider(provider: string) {
    setCollapsedProviders((current) => {
      const next = new Set(current);
      if (next.has(provider)) next.delete(provider);
      else next.add(provider);
      return next;
    });
  }

  function change(
    provider: string,
    model: SubagentModel,
    patch: { enabled?: boolean; enabled_variants?: string[] },
  ) {
    const enabled = patch.enabled ?? model.enabled;
    const enabledVariants = patch.enabled_variants ?? model.enabled_variants;
    pending.begin(keyOf(provider, model.id));
    getClient()?.setSubagentModel(provider, model.id, enabled, enabledVariants);
  }

  return (
    <Show when={catalog()}>
      <SubHeading>Sub-agent models</SubHeading>
      <p class="mb-2 text-xs leading-snug text-muted-foreground">
        Choose which models and reasoning levels an Agent may use for Sub-agents.
      </p>
      <div class="flex flex-col gap-3">
        <For each={catalog()?.providers ?? []}>
          {(group) => (
            <div>
              <button
                type="button"
                aria-expanded={!isCollapsed(group.provider)}
                aria-controls={providerPanelId(group.provider)}
                aria-label={`${isCollapsed(group.provider) ? "Expand" : "Collapse"} ${group.label} models`}
                onClick={() => toggleProvider(group.provider)}
                class="mb-1 flex min-h-8 w-full items-center justify-between rounded-lg text-xs font-medium text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring [@media(pointer:coarse)]:min-h-11"
              >
                <span>{group.label}</span>
                <ChevronDown
                  aria-hidden="true"
                  class="size-3.5 transition-transform duration-200 ease-out"
                  classList={{ "-rotate-90": isCollapsed(group.provider) }}
                />
              </button>
              <Show when={!isCollapsed(group.provider)}>
                <Group
                  id={providerPanelId(group.provider)}
                  class="divide-y divide-border"
                >
                  <For each={group.models}>
                    {(model) => (
                      <SubagentModelRow
                        provider={group.provider}
                        model={model}
                        pending={pending.isPending(keyOf(group.provider, model.id))}
                        onChange={(patch) => change(group.provider, model, patch)}
                      />
                    )}
                  </For>
                </Group>
              </Show>
            </div>
          )}
        </For>
      </div>
    </Show>
  );
}

/** Each subsection self-hides when its store field is null (older hosts), so
 * the whole section collapses to nothing on a host that reports neither. */
export function ModelsSection(): JSX.Element {
  return (
    <Show when={state.model || state.subagentModels}>
      <SectionHeader id="settings-models">Models</SectionHeader>
      <MainAgentModel />
      <SubagentModels />
    </Show>
  );
}
