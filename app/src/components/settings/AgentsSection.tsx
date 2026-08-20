// Settings → Agents: the two resident agents (ADR-0015) and the catalog the
// main Agent may spawn Sub-agents from. Main agent first — the one
// interlocutor — then the ephemeral fork, then the Sub-agent models. Every
// control is host-backed: it reflects a broadcast snapshot and settles from the
// next one, never from an optimistic local write.
import { ChevronDown } from "lucide-solid";
import { createEffect, createSignal, For, type JSX, Show } from "solid-js";
import { createPendingKeys } from "../../lib/pending";
import type { ModelSelection, SubagentModel } from "../../protocol";
import { state } from "../../store/store";
import { getClient } from "../../ws/client";
import {
  AgentModelRows,
  AgentProviderRow,
  createAgentPending,
  PromptEditor,
  providerLabel,
  settleOnProtocolError,
} from "./agent-config";
import { ForkAgentSection } from "./ForkAgentSection";
import { titleCase } from "./prefs";
import { Group, SubHeading, Toggle } from "./rows";

const EMPTY_PROMPT = { text: "", is_default: true };

/** The main Agent's provider, model and prompt.
 *
 * Two changes here take effect on two different clocks, and the copy says which
 * is which: a model change reaches the live session and holds from its next
 * turn, while a provider change is stored now and the resident session keeps
 * running on the provider the host booted with. */
function MainAgent() {
  const pending = createAgentPending();
  const snapshot = () => state.model;
  const current = () => snapshot()?.current;
  const providerId = () => snapshot()?.provider_id;
  const placeholder = () =>
    state.providers?.instances.find((instance) => instance.id === providerId())?.default_model;

  // Which selection we're awaiting, so a settled control is settled by the
  // truth rather than by the send. Bounded: a matching broadcast, an error
  // frame, or the timeout — the value is NOT guaranteed to be echoed (the host
  // only broadcasts an actual change).
  const [awaited, setAwaited] = createSignal<ModelSelection | null>(null);
  createEffect(() => {
    const selection = awaited();
    const settled = current();
    if (selection && settled && settled.id === selection.id && settled.variant === selection.variant) {
      setAwaited(null);
      pending.settleAll();
    }
  });

  // The main prompt settles from the authoritative prompts frame, like the
  // fork's does. Equal snapshots are still acknowledgements.
  createEffect(() => {
    const prompts = state.prompts;
    if (!prompts) return;
    void state.promptsRevision;
    void prompts.agent.text;
    void prompts.agent.is_default;
    pending.settle("agent-prompt");
  });

  function select(selection: ModelSelection) {
    setAwaited(selection);
    getClient()?.setModel(selection.id, selection.variant);
  }

  /** The running session boots on one provider and stays there. When the stored
   * choice has moved on, say so plainly and once — no toast, no alarm colour. */
  const bootedElsewhere = () => {
    const booted = state.providers?.booted_provider_id;
    const chosen = providerId();
    return booted && chosen && booted !== chosen ? providerLabel(booted) : null;
  };

  return (
    <>
      <SubHeading>Main agent</SubHeading>
      <Group class="divide-y divide-border">
        <Show when={state.providers}>
          <div>
            <AgentProviderRow
              slot="main"
              name="Main agent"
              providerId={providerId()}
              pending={pending}
            />
            <Show when={bootedElsewhere()}>
              {(booted) => (
                <p class="pb-3 text-xs leading-snug text-muted-foreground">
                  Saved. The running Agent stays on {booted()} until the host restarts.
                </p>
              )}
            </Show>
          </div>
        </Show>
        <Show when={current()}>
          {(selection) => (
            <div>
              <div class="divide-y divide-border">
                <AgentModelRows
                  name="Main agent"
                  freeText={snapshot()?.free_text_model === true}
                  current={selection()}
                  available={snapshot()?.available ?? []}
                  placeholder={placeholder()}
                  pending={pending}
                  modelKey="model"
                  variantKey="variant"
                  onSelect={select}
                  onFreeText={(modelId) => select({ id: modelId, variant: selection().variant })}
                />
              </div>
              <p class="pb-3 text-xs leading-snug text-muted-foreground">
                Applies from the Agent's next turn.
              </p>
            </div>
          )}
        </Show>
        <Show when={state.prompts}>
          <div>
            <p class="pt-3 text-xs leading-snug text-muted-foreground">
              The editable body applies from the next turn. Host configuration is appended
              automatically and is not part of this field.
            </p>
            <PromptEditor
              label="Main agent system prompt"
              doc={() => state.prompts?.agent ?? EMPTY_PROMPT}
              pending={pending}
              pendingKey="agent-prompt"
              onSave={(text) => getClient()?.setAgentPrompt(text)}
            />
          </div>
        </Show>
      </Group>
    </>
  );
}

/** One sub-agent model row: identity + master toggle + a quiet multi-select
 * variant field. Any change sends the FULL row state via
 * `set_subagent_model`; `pending` (shared across the subsection) fades the row
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

/** The Sub-agent model catalog, grouped by provider. Hidden when the host
 * reports no catalog (older hosts). */
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
      <SubHeading>Sub-agents</SubHeading>
      <p class="mb-2 text-xs leading-snug text-muted-foreground">
        Choose which models and reasoning levels an Agent may use for Sub-agents. Claude is
        available to Sub-agents only — it never runs the main Agent or the fork.
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
 * the tab collapses to nothing on a host that reports none of them. */
export function AgentsSection(): JSX.Element {
  const fork = () => state.prompts?.fork;
  return (
    <>
      <Show when={state.model || state.prompts}>
        <MainAgent />
      </Show>
      <Show when={fork()}>
        {(config) => <ForkAgentSection fork={config} />}
      </Show>
      <SubagentModels />
    </>
  );
}
