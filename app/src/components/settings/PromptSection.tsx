// Settings → Prompt: the effective main-Agent prompt plus the persisted
// incoming-event fork model and prompt. The Host remains authoritative: drafts
// stay local until Save/Reset, then settle from the full prompts_changed frame.
import { LoaderCircle } from "lucide-solid";
import { createEffect, createSignal, type JSX, Show, untrack } from "solid-js";
import { createPendingKeys, type PendingKeys } from "../../lib/pending";
import type { ForkAgentConfig, ModelSelection, PromptDoc } from "../../protocol";
import { state } from "../../store/store";
import { getClient } from "../../ws/client";
import { Button } from "../ui/button";
import { Group, SectionHeader, Select, SubHeading } from "./rows";
import { titleCase } from "./prefs";

const EMPTY_PROMPT: PromptDoc = { text: "", is_default: true };

function settleOnProtocolError(pending: PendingKeys): void {
  let seen = untrack(() => state.protocolError);
  createEffect(() => {
    const error = state.protocolError;
    if (error === seen) return;
    seen = error;
    if (error !== null) pending.settleAll();
  });
}

function PromptEditor(props: {
  label: string;
  doc: () => PromptDoc;
  pending: PendingKeys;
  pendingKey: string;
  onSave: (text: string) => void;
  rows?: number;
}) {
  const [draft, setDraft] = createSignal(props.doc().text);
  let serverText = props.doc().text;

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

  return (
    <div class="py-3">
      <div class="mb-2 flex items-center justify-between gap-3">
        <label class="text-sm text-foreground" for={`${props.pendingKey}-editor`}>
          {props.label}
        </label>
        <Show when={busy()}>
          <LoaderCircle class="size-3.5 animate-spin text-muted-foreground" aria-label="Saving" />
        </Show>
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
      <div class="mt-2 flex items-center gap-2">
        <Button
          size="sm"
          class="h-9"
          aria-label={`Save ${props.label}`}
          disabled={busy() || !dirty()}
          onClick={() => save(draft())}
        >
          Save
        </Button>
        <Button
          size="sm"
          variant="ghost"
          class="h-9"
          aria-label={`Reset ${props.label} to default`}
          disabled={busy() || (props.doc().is_default && !dirty())}
          onClick={reset}
        >
          Reset to default
        </Button>
        <Show when={props.doc().is_default}>
          <span class="text-xs text-muted-foreground">Bundled default</span>
        </Show>
      </div>
    </div>
  );
}

function ForkAgent(props: { fork: () => ForkAgentConfig; pending: PendingKeys }) {
  const current = () => props.fork().current;
  const available = () => props.fork().available;
  const selectedModel = () => available().find((model) => model.id === current().id);
  const busy = () => props.pending.any();

  function send(selection: ModelSelection, key: "fork-model" | "fork-variant") {
    props.pending.begin(key);
    getClient()?.setForkModel(selection.id, selection.variant);
  }

  function changeModel(id: string) {
    if (id === current().id) return;
    const model = available().find((candidate) => candidate.id === id);
    if (model) send({ id, variant: model.default_variant }, "fork-model");
  }

  function changeVariant(variant: string) {
    if (variant === current().variant) return;
    send({ id: current().id, variant }, "fork-variant");
  }

  return (
    <>
      <SubHeading>Fork agent</SubHeading>
      <p class="mb-2 text-xs leading-snug text-muted-foreground">
        Runs once per incoming event to triage it. This configuration is stored for the fork runtime
        and does not affect the current Agent.
      </p>
      <Group class="divide-y divide-border">
        <div class="flex items-center justify-between gap-3 py-3">
          <span class="text-sm text-foreground">Model</span>
          <Select
            ariaLabel="Fork agent model"
            class="w-[10.5rem] shrink-0"
            disabled={busy()}
            value={current().id}
            onChange={changeModel}
            options={available().map((model) => ({ value: model.id, label: model.label }))}
          />
        </div>
        <div class="flex items-center justify-between gap-3 py-3">
          <span class="text-sm text-foreground">Reasoning</span>
          <Select
            ariaLabel="Fork agent reasoning variant"
            class="w-[10.5rem] shrink-0"
            disabled={busy() || !selectedModel()}
            value={current().variant}
            onChange={changeVariant}
            options={(selectedModel()?.variants ?? []).map((variant) => ({
              value: variant,
              label: titleCase(variant),
            }))}
          />
        </div>
        <PromptEditor
          label="Fork agent prompt"
          doc={() => props.fork().prompt}
          pending={props.pending}
          pendingKey="fork-prompt"
          rows={10}
          onSave={(text) => getClient()?.setForkPrompt(text)}
        />
      </Group>
    </>
  );
}

export function PromptSection(): JSX.Element {
  const pending = createPendingKeys();
  settleOnProtocolError(pending);
  const agent = () => state.prompts?.agent ?? EMPTY_PROMPT;
  const fork = () => state.prompts?.fork;

  createEffect(() => {
    const prompts = state.prompts;
    if (!prompts) return;
    // Equal snapshots are still acknowledgements. The store increments this
    // once per prompts_changed frame because equal nested fields do not notify
    // Solid effects on their own.
    void state.promptsRevision;
    // Solid stores merge object slices in place. Subscribe to the fields a
    // full prompts_changed frame may replace rather than only the stable root
    // proxy, then settle every control from that authoritative snapshot.
    void prompts.agent.text;
    void prompts.agent.is_default;
    void prompts.fork?.current.id;
    void prompts.fork?.current.variant;
    void prompts.fork?.prompt.text;
    void prompts.fork?.prompt.is_default;
    pending.settleAll();
  });

  return (
    <Show when={state.prompts}>
      <SectionHeader id="settings-prompt">Prompt</SectionHeader>
      <p class="mb-2 text-xs leading-snug text-muted-foreground">
        The editable body applies from the next turn. Host configuration is appended automatically
        and is not part of this field.
      </p>
      <Group class="divide-y divide-border">
        <PromptEditor
          label="Main agent system prompt"
          doc={agent}
          pending={pending}
          pendingKey="agent-prompt"
          onSave={(text) => getClient()?.setAgentPrompt(text)}
        />
      </Group>
      <Show when={fork()}>
        <ForkAgent fork={() => fork() as ForkAgentConfig} pending={pending} />
      </Show>
    </Show>
  );
}
