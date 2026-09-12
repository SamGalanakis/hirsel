import { ChevronDown, LoaderCircle } from "@/components/ui/icons";
import { createEffect, createSignal, For, Show } from "solid-js";
import { type JSX } from "@solidjs/web";
import { createPendingKeys } from "../../lib/pending";
import type { ModelSelection, SubagentModel, SubagentNativeWorker } from "../../protocol";
import { state } from "../../store/store";
import { getClient } from "../../ws/client";
import {
  AgentModelRows,
  AgentProviderRow,
  agentModelView,
  createAgentPending,
  PromptEditor,
  providerLabel,
  settleOnProtocolError,
} from "./agent-config";
import { Button } from "../ui/button";
import { Input } from "../ui/input";
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
  const [selectedProviderId, setSelectedProviderId] = createSignal(providerId());
  createEffect(() => ({ stored: providerId(), busy: pending.isPending("main-provider") }), ({ stored, busy }) => {
    if (!busy) setSelectedProviderId(stored);
  });
  const modelView = () => {
    const stored = current();
    if (!stored) return null;
    return agentModelView(
      "main",
      selectedProviderId(),
      providerId(),
      stored,
      snapshot()?.available ?? [],
      snapshot()?.free_text_model === true,
    );
  };

  // Which selection we're awaiting, so a settled control is settled by the
  // truth rather than by the send. Bounded: a matching broadcast, an error
  // frame, or the timeout — the value is NOT guaranteed to be echoed (the host
  // only broadcasts an actual change).
  const [awaited, setAwaited] = createSignal<ModelSelection | null>(null);
  createEffect(() => ({ selection: awaited(), settled: current() ? { id: current()!.id, variant: current()!.variant } : null }), ({ selection, settled }) => {
    if (selection && settled && settled.id === selection.id && settled.variant === selection.variant) {
      setAwaited(null);
      pending.settleAll();
    }
  });

  // The main prompt settles from the authoritative prompts frame, like the
  // fork's does. Equal snapshots are still acknowledgements.
  createEffect(() => state.prompts ? [state.promptsRevision, state.prompts.agent.text, state.prompts.agent.is_default] : null, (prompts) => {
    if (prompts) pending.settle("agent-prompt");
  });

  function select(selection: ModelSelection) {
    const selectedProvider = selectedProviderId();
    if (!selectedProvider) return;
    setAwaited(selection);
    getClient()?.setModel(selectedProvider, selection.id, selection.variant);
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
              selectedProviderId={selectedProviderId()}
              pending={pending}
              onProviderChange={setSelectedProviderId}
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
        <Show when={modelView()}>
          {(view) => (
            <div>
              <div class="divide-y divide-border">
                <AgentModelRows
                  name="Main agent"
                  freeText={view().freeText}
                  current={view().current}
                  available={view().available}
                  placeholder={view().placeholder}
                  pending={pending}
                  modelKey="model"
                  variantKey="variant"
                  onSelect={select}
                  onFreeText={(modelId) =>
                    select({ id: modelId, variant: view().current.variant })
                  }
                />
              </div>
              {/* One caption, honest about which clock this edit is on: the
                  model reaches the live session only while the stored provider
                  IS the booted one. Otherwise it is stored for the restart,
                  exactly like the provider choice above it. */}
              <p class="pb-3 text-xs leading-snug text-muted-foreground">
                {bootedElsewhere()
                  ? "Takes effect when the host restarts."
                  : "Applies from the Agent's next turn."}
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
              caption="Applies from the Agent's next turn. Host configuration is appended automatically and is not part of this field."
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
      class={["py-3 transition-opacity", { "opacity-60": props.pending }]}

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
        aria-disabled={(!props.model.enabled) ? "true" : "false"}
        class={["mt-3 flex flex-wrap gap-1.5 transition-opacity", { "opacity-45": !props.model.enabled }]}

      >
        <For each={props.model.variants}>
          {(variant) => {
            const active = () => selected(variant);
            const isLast = () => active() && props.model.enabled_variants.length === 1;
            return (
              <button
                type="button"
                aria-pressed={(active()) ? "true" : "false"}
                aria-label={`${active() ? "Disable" : "Enable"} ${props.model.label} ${variant} variant`}
                disabled={props.pending || !props.model.enabled || isLast()}
                title={isLast() ? "At least one variant must stay enabled" : undefined}
                onClick={() => toggleVariant(variant)}
                class={["min-h-8 rounded-full border px-2.5 text-xs font-medium outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-55 [@media(pointer:coarse)]:min-h-11 [@media(pointer:coarse)]:px-3.5", {
                  "border-primary/60 bg-primary/10 text-foreground": active(),
                  "border-border bg-surface text-muted-foreground hover:border-input hover:text-foreground":
                    !active(),
                }]}

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


/** Where a native-worker delegation actually lands, said plainly. Three honest
 * states: nothing can host it, the default instance hosts it, or the roster has
 * instances but not the default one — in which case each delegation names its
 * own, and saying so is more use than a badge that reads "unavailable". */
function nativeWorkerRoute(worker: SubagentNativeWorker): string {
  if (worker.unavailable_reason) return worker.unavailable_reason;
  const label = (id: string) =>
    state.providers?.instances.find((instance) => instance.id === id)?.label ?? id;
  if (worker.provider_id) return `Runs on ${label(worker.provider_id)}.`;
  return `Each delegation names its provider: ${worker.eligible_provider_ids
    .map(label)
    .join(", ")}.`;
}

/** The native worker row: one in-process coding worker, not a CLI lane. It has
 * no curated model list and no reasoning variants, so the row is an enable
 * switch plus the free-text model its default route opens on — the same
 * full-state upsert and broadcast settle as the rows above it. */
function NativeWorkerRow(props: {
  worker: SubagentNativeWorker;
  pending: boolean;
  onChange: (next: { enabled: boolean; model?: string }) => void;
}) {
  const [draft, setDraft] = createSignal(props.worker.model_override ?? "");
  let settled = props.worker.model_override ?? "";
  createEffect(() => props.worker.model_override ?? "", (next) => {
    if (next !== settled) {
      settled = next;
      setDraft(next);
    }
  });

  const override = () => {
    const trimmed = draft().trim();
    return trimmed.length > 0 ? trimmed : undefined;
  };

  return (
    <>
      <div class={["py-3 transition-opacity", { "opacity-60": props.pending }]}>
        <div class="flex items-center gap-3">
          {/* The group heading already names the worker, so the row's own
              identity is the model this route opens on — the one fact the
              toggle is actually switching on and off. */}
          <div class="min-w-0 flex-1">
            <div class="truncate font-mono text-sm text-foreground">{props.worker.model}</div>
            <div class="mt-0.5 text-xs leading-snug text-muted-foreground">
              {nativeWorkerRoute(props.worker)}
            </div>
          </div>
          <Toggle
            ariaLabel={`Enable ${props.worker.label}`}
            checked={props.worker.enabled}
            disabled={props.pending}
            onChange={(enabled) => props.onChange({ enabled, model: override() })}
          />
        </div>
      </div>
      <div
        class={["py-3 transition-opacity", { "opacity-45": !props.worker.enabled }]}
        aria-disabled={(!props.worker.enabled) ? "true" : "false"}
      >
        <div class="flex flex-wrap items-center justify-between gap-3">
          <div class="flex min-w-0 items-center gap-2">
            <span class="text-sm text-foreground">Model</span>
            <Show when={props.pending}>
              <LoaderCircle
                class="size-3.5 shrink-0 animate-spin text-muted-foreground"
                aria-label="Saving"
              />
            </Show>
          </div>
          <div class="flex shrink-0 items-center gap-2">
            <Input
              aria-label={`${props.worker.label} model id`}
              class="h-9 w-[12rem] rounded-lg border border-border bg-surface px-2.5 font-mono text-xs text-foreground transition-colors hover:border-input focus-visible:ring-2 focus-visible:ring-ring"
              value={draft()}
              disabled={props.pending || !props.worker.enabled}
              placeholder={props.worker.default_model}
              autocomplete="off"
              spellcheck={false}
              onInput={(event) => setDraft(event.currentTarget.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter")
                  props.onChange({ enabled: props.worker.enabled, model: override() });
              }}
            />
            <Button
              size="sm"
              class="h-9"
              aria-label={`Save ${props.worker.label} model id`}
              disabled={props.pending || !props.worker.enabled}
              onClick={() => props.onChange({ enabled: props.worker.enabled, model: override() })}
            >
              Save
            </Button>
          </div>
        </div>
        <p class="mt-1.5 text-xs leading-snug text-muted-foreground">
          Leave it empty to use the shipped default. A delegation that names its own model still
          wins.
        </p>
      </div>
    </>
  );
}

/** The Sub-agent model catalog, grouped by provider. Hidden when the host
 * reports no catalog (older hosts). */
function SubagentModels() {
  const catalog = () => state.subagentModels;
  const [collapsedProviders, setCollapsedProviders] = createSignal<Set<string>>(new Set());
  const catalogVersion = () => {
    const current = catalog();
    if (!current) return "";
    const worker = current.native_worker;
    return [
      ...current.providers.flatMap((provider) =>
        provider.models.map(
          (model) =>
            `${provider.provider}:${model.id}:${model.enabled}:${model.enabled_variants.join(",")}`,
        ),
      ),
      // The native worker settles on the same broadcast, so its state is part
      // of the version the pending set is cleared by.
      `lash:${worker?.enabled}:${worker?.model}:${worker?.provider_id}:${worker?.eligible_provider_ids.join(",")}`,
    ].join("|");
  };

  // Rows awaiting the broadcast, keyed `provider\u0000modelId`. Cleared whenever
  // the catalog content changes (a broadcast settled the truth); Solid's store
  // updates nested catalog objects in place, so tracking the root reference is
  // insufficient. Coarse but correct: the broadcast is authoritative for all.
  // A rejected or no-op write produces no broadcast at all, so each key is also
  // bounded by the error frame and by its timeout.
  const pending = createPendingKeys();
  createEffect(catalogVersion, () => {
    pending.settleAll();
  });
  settleOnProtocolError(pending);

  const keyOf = (provider: string, id: string) => `${provider}\u0000${id}`;
  const providerPanelId = (provider: string) => `subagent-provider-${provider}`;
  const isCollapsed = (provider: string) => collapsedProviders().has(provider);
  const NATIVE_WORKER_GROUP = "native-worker";

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
      <SubHeading>Delegation models</SubHeading>
      <p class="mb-2 text-xs leading-snug text-muted-foreground">
        Choose models and reasoning levels for focused child Threads. Claude and Codex run
        delegated work through their CLIs; the native worker runs inside this host on a configured
        provider. Progress and results return to the parent conversation either way.
      </p>
      <div class="flex flex-col gap-3">
        <For each={catalog()?.providers ?? []}>
          {(group) => (
            <div>
              <button
                type="button"
                aria-expanded={(!isCollapsed(group.provider)) ? "true" : "false"}
                aria-controls={providerPanelId(group.provider)}
                aria-label={`${isCollapsed(group.provider) ? "Expand" : "Collapse"} ${group.label} models`}
                onClick={() => toggleProvider(group.provider)}
                class="mb-1 flex min-h-8 w-full items-center justify-between rounded-lg text-xs font-medium text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring [@media(pointer:coarse)]:min-h-11"
              >
                <span>{group.label}</span>
                <ChevronDown
                  aria-hidden="true"
                  class={["size-3.5 transition-transform duration-200 ease-out", { "-rotate-90": isCollapsed(group.provider) }]}

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
        <Show when={catalog()?.native_worker}>
          {(worker) => (
            <div>
              <button
                type="button"
                aria-expanded={(!isCollapsed(NATIVE_WORKER_GROUP)) ? "true" : "false"}
                aria-controls={providerPanelId(NATIVE_WORKER_GROUP)}
                aria-label={`${isCollapsed(NATIVE_WORKER_GROUP) ? "Expand" : "Collapse"} ${worker().label}`}
                onClick={() => toggleProvider(NATIVE_WORKER_GROUP)}
                class="mb-1 flex min-h-8 w-full items-center justify-between rounded-lg text-xs font-medium text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring [@media(pointer:coarse)]:min-h-11"
              >
                <span>{worker().label}</span>
                <ChevronDown
                  aria-hidden="true"
                  class={["size-3.5 transition-transform duration-200 ease-out", { "-rotate-90": isCollapsed(NATIVE_WORKER_GROUP) }]}

                />
              </button>
              <Show when={!isCollapsed(NATIVE_WORKER_GROUP)}>
                <Group
                  id={providerPanelId(NATIVE_WORKER_GROUP)}
                  class="divide-y divide-border"
                >
                  <NativeWorkerRow
                    worker={worker()}
                    pending={pending.isPending(NATIVE_WORKER_GROUP)}
                    onChange={(next) => {
                      pending.begin(NATIVE_WORKER_GROUP);
                      getClient()?.setNativeWorker(next.enabled, next.model);
                    }}
                  />
                </Group>
              </Show>
            </div>
          )}
        </Show>
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
