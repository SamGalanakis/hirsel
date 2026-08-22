// Settings → Agents → Fork agent: the provider, model and prompt of the
// ephemeral fork spawned once per incoming event (ADR-0015). Stored
// configuration — the running Agent is untouched by any of it.
import { createEffect, type JSX } from "solid-js";
import type { ForkAgentConfig } from "../../protocol";
import { state } from "../../store/store";
import { getClient } from "../../ws/client";
import {
  AgentModelRows,
  AgentProviderRow,
  createAgentPending,
  PromptEditor,
} from "./agent-config";
import { Group, SubHeading } from "./rows";

export function ForkAgentSection(props: { fork: () => ForkAgentConfig }): JSX.Element {
  const pending = createAgentPending();
  const current = () => props.fork().current;
  const providerId = () => props.fork().provider_id;
  const placeholder = () =>
    state.providers?.instances.find((instance) => instance.id === providerId())?.default_model;

  // Every fork control settles from the authoritative prompts frame. Equal
  // snapshots are still acknowledgements, so the revision counter is part of
  // what this effect tracks (equal nested fields do not notify on their own).
  createEffect(() => {
    const fork = state.prompts?.fork;
    if (!fork) return;
    void state.promptsRevision;
    void fork.current.id;
    void fork.current.variant;
    void fork.provider_id;
    void fork.prompt.text;
    void fork.prompt.is_default;
    pending.settleAll();
  });

  return (
    <>
      <SubHeading>Fork agent</SubHeading>
      <p class="mb-2 text-xs leading-snug text-muted-foreground">
        Runs once per incoming event to triage it. This provider, model and prompt are stored for
        the fork runtime and do not affect the current Agent.
      </p>
      <Group class="divide-y divide-border">
        <AgentProviderRow slot="fork" name="Fork agent" providerId={providerId()} pending={pending} />
        <AgentModelRows
          name="Fork agent"
          freeText={props.fork().free_text_model === true}
          current={current()}
          available={props.fork().available}
          placeholder={placeholder()}
          pending={pending}
          modelKey="fork-model"
          variantKey="fork-variant"
          onSelect={(selection) => getClient()?.setForkModel(selection.id, selection.variant)}
          onFreeText={(modelId) => getClient()?.setForkModel(modelId, current().variant)}
        />
        <PromptEditor
          label="Fork agent prompt"
          doc={() => props.fork().prompt}
          pending={pending}
          pendingKey="fork-prompt"
          caption="Stored for the fork runtime. The running Agent is unaffected."
          rows={10}
          onSave={(text) => getClient()?.setForkPrompt(text)}
        />
      </Group>
    </>
  );
}
