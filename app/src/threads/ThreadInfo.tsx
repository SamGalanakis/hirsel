import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { type JSX } from "@solidjs/web";
import { ArtifactCard } from "../artifacts/ArtifactSurface";
import { Markdown } from "../components/Markdown";
import { AgentModelRows, agentModelView, agentProviders, providerLabel } from "../components/settings/agent-config";
import { titleCase } from "../components/settings/prefs";
import { Select } from "../components/settings/rows";
import { SquarePen } from "../components/ui/icons";
import { formatRelativeTime } from "../lib/format";
import { createPendingKeys } from "../lib/pending";
import type { AvailableModel } from "../protocol";
import { relatedState } from "../related/store";
import { openProcesses, state } from "../store/store";
import { ThreadAvatar } from "./ThreadAvatar";
import { ThreadLink } from "./ThreadRef";
import { ThreadStatus } from "./ThreadStatus";
import { openThreadIconPicker } from "./icon-picker";
import { setThreadState, threadAction, threadState } from "./store";
import type { Thread, ThreadExecutionTarget } from "./types";

const quiet = "inline-flex h-8 items-center justify-center gap-1.5 rounded-md px-2 text-meta text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-40 pointer-coarse:h-11 pointer-coarse:text-sm";
const action = "inline-flex h-8 items-center justify-center rounded-md px-3 text-sm transition-colors hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-40 pointer-coarse:h-11";
const primary = `${action} bg-primary text-primary-foreground hover:bg-primary/90`;
const field = "w-full rounded-md border border-border bg-transparent px-2 py-1.5 text-sm placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring";

/** The request error this Thread is carrying, shown beside the control that
 * caused it so a refused edit stays open on the draft the Owner typed. */
function requestFailure(threadId: number): string | null {
  const failure = threadState.error;
  return failure && failure.operation === "request" && failure.threadId === threadId ? failure.detail : null;
}

/** One Owner edit in flight. It settles on the Host's new revision (the edit
 * landed, so the editor closes) or on a refusal (the editor stays, holding the
 * draft). Nothing here is optimistic: the pane shows the stored Thread until
 * the Host says otherwise. */
function createEdit(thread: () => Thread, historyId: () => string) {
  const [editing, setEditing] = createSignal(false);
  const [saving, setSaving] = createSignal(false);
  let sent: number | null = null;
  createEffect(() => thread().revision, revision => {
    if (sent !== null && revision !== sent) { sent = null; setSaving(false); setEditing(false); }
  });
  createEffect(() => requestFailure(thread().id), failure => {
    if (sent !== null && failure) { sent = null; setSaving(false); }
  });
  const submit = (name: string, data: unknown) => {
    // A previous refusal is this control's own stale news; clear it so the
    // retry is judged by its own answer.
    setThreadState(draft => { draft.error = null; });
    sent = thread().revision;
    setSaving(true);
    threadAction(historyId(), thread().id, name, data, thread().revision);
  };
  return { editing, saving, setEditing, submit };
}

function Fact(props: { label: string; children: JSX.Element }) {
  return <div class="flex gap-3 py-1.5" data-slot="thread-fact" data-fact={props.label}>
    <dt class="w-28 shrink-0 text-meta text-muted-foreground">{props.label}</dt>
    <dd class="min-w-0 flex-1 text-sm">{props.children}</dd>
  </div>;
}

function Failure(props: { threadId: number }) {
  return <Show when={requestFailure(props.threadId)}>{detail => <p role="alert" class="text-meta text-status-danger">{detail()}</p>}</Show>;
}

function absolute(ts: string): string {
  const parsed = Date.parse(ts);
  return Number.isFinite(parsed) ? new Date(parsed).toISOString().replace("T", " ").replace(/\..*/, " UTC") : ts;
}

/** What the "Runs on" row says, for each shape the Host sends. */
export function executionLabel(execution: ThreadExecutionTarget | null | undefined): { text: string; muted: boolean } {
  if (!execution) {
    const coordinator = [providerLabel(state.model?.provider_id), state.model?.current.id].filter(Boolean).join(" · ");
    return { text: coordinator ? `Default coordinator · ${coordinator}` : "Default coordinator", muted: true };
  }
  if (execution.kind === "host") return { text: ["Coordinator", providerLabel(execution.provider_id) || execution.provider_id, execution.model].join(" · "), muted: false };
  if (execution.kind === "lash") {
    const worker = state.subagentModels?.native_worker.label ?? "Native worker";
    return { text: [worker, providerLabel(execution.provider_id) || execution.provider_id, execution.model, titleCase(execution.variant)].join(" · "), muted: false };
  }
  const group = state.subagentModels?.providers.find(provider => provider.provider === execution.agent);
  return { text: [group?.label ?? titleCase(execution.agent), execution.model, titleCase(execution.variant)].join(" · "), muted: false };
}

function TitleRow(props: { thread: Thread; historyId: string }) {
  const edit = createEdit(() => props.thread, () => props.historyId);
  const [draft, setDraft] = createSignal(props.thread.title);
  const open = () => { setDraft(props.thread.title); edit.setEditing(true); };
  const save = () => { if (draft().trim() && draft().trim() !== props.thread.title) edit.submit("set_title", { title: draft().trim() }); else edit.setEditing(false); };
  return <Show when={edit.editing()} fallback={<div class="flex min-w-0 items-center gap-0.5">
    <h2 class="min-w-0 truncate text-lg font-semibold leading-tight">{props.thread.title}</h2>
    <button type="button" class={`${quiet} shrink-0`} aria-label="Rename thread" title="Rename" onClick={open}><SquarePen class="size-3.5" /></button>
  </div>}>
    <div class="flex min-w-0 flex-col gap-1.5">
      <input class={field} aria-label="Thread title" value={draft()} disabled={edit.saving()} autofocus
        onInput={event => setDraft(event.currentTarget.value)}
        onKeyDown={event => {
          if (event.key === "Enter") { event.preventDefault(); save(); }
          if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); edit.setEditing(false); }
        }} />
      <div class="flex items-center gap-1.5">
        <button type="button" class={primary} aria-label="Save title" disabled={edit.saving() || !draft().trim()} onClick={save}>Save</button>
        <button type="button" class={action} aria-label="Cancel rename" disabled={edit.saving()} onClick={() => edit.setEditing(false)}>Cancel</button>
      </div>
      <Failure threadId={props.thread.id} />
    </div>
  </Show>;
}

function DescriptionSection(props: { thread: Thread; historyId: string }) {
  const edit = createEdit(() => props.thread, () => props.historyId);
  const [draft, setDraft] = createSignal(props.thread.description);
  const open = () => { setDraft(props.thread.description); edit.setEditing(true); };
  const save = () => { if (draft() !== props.thread.description) edit.submit("set_description", { description: draft() }); else edit.setEditing(false); };
  return <section class="flex flex-col gap-1.5" data-slot="thread-description">
    <Show when={edit.editing()} fallback={<>
      <div class="flex items-center gap-0.5">
        <h3 class="text-meta font-medium uppercase tracking-wider text-muted-foreground">Description</h3>
        <button type="button" class={`${quiet} shrink-0`} aria-label="Edit description" title="Edit description" onClick={open}><SquarePen class="size-3.5" /></button>
      </div>
      <Show when={props.thread.description} fallback={<p class="text-sm text-muted-foreground">No description yet — add one</p>}>
        <Markdown>{props.thread.description}</Markdown>
      </Show>
    </>}>
      <textarea class={`${field} min-h-28 resize-y`} aria-label="Thread description" value={draft()} disabled={edit.saving()} autofocus
        onInput={event => setDraft(event.currentTarget.value)}
        onKeyDown={event => { if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); edit.setEditing(false); } }} />
      <div class="flex items-center gap-1.5">
        <button type="button" class={primary} aria-label="Save description" disabled={edit.saving()} onClick={save}>Save</button>
        <button type="button" class={action} aria-label="Cancel description edit" disabled={edit.saving()} onClick={() => edit.setEditing(false)}>Cancel</button>
      </div>
      <Failure threadId={props.thread.id} />
    </Show>
  </section>;
}

type Backend = "default" | "host" | `cli:${string}` | "lash";

/** The model id a freshly chosen coordinator provider starts on. A free-text
 * endpoint has nothing to pick from, so it opens on the provider's own default
 * rather than an empty field; a curated one resolves its first model below. */
function coordinatorSeedModel(providerId: string): string {
  const provider = state.providers?.instances.find(instance => instance.id === providerId);
  return provider?.selection?.mode === "free_text" ? provider.default_model ?? "" : "";
}

/** Where the next turn runs. The choices are the delegation catalog's own — the
 * same agents, models and variants `threads.delegate` accepts — and the Host
 * validates the request against it again. */
function RunsOnRow(props: { thread: Thread; historyId: string }) {
  const edit = createEdit(() => props.thread, () => props.historyId);
  const catalog = () => state.subagentModels;
  const worker = () => {
    const native = catalog()?.native_worker;
    return native && native.enabled && !native.unavailable_reason ? native : null;
  };
  // Every agent-selectable roster instance can host the coordinator: the same
  // rule the Settings provider picker obeys, and the same one the Host
  // re-validates the request against.
  const coordinators = () => agentProviders();
  const defaultProviderId = (backend: Backend): string => {
    if (backend === "host") {
      const configured = state.model?.provider_id;
      return (configured && coordinators().some(instance => instance.id === configured) ? configured : coordinators()[0]?.id) ?? "";
    }
    if (backend === "lash") return worker()?.provider_id ?? worker()?.eligible_provider_ids[0] ?? "";
    return "";
  };
  const initial = (): { backend: Backend; model: string; variant: string; providerId: string } => {
    const execution = props.thread.execution;
    if (execution?.kind === "cli") return { backend: `cli:${execution.agent}`, model: execution.model, variant: execution.variant, providerId: "" };
    if (execution?.kind === "lash") return { backend: "lash", model: execution.model, variant: "default", providerId: execution.provider_id };
    if (execution?.kind === "host") return { backend: "host", model: execution.model, variant: "default", providerId: execution.provider_id };
    return { backend: "default", model: "", variant: "", providerId: defaultProviderId("default") };
  };
  const [draft, setDraft] = createSignal(initial());
  // The model rows are the Settings ones; they mark themselves pending on every
  // change, and here the change is local, so settle immediately.
  const pending = createPendingKeys();
  const options = createMemo(() => [
    { value: "default" as Backend, label: "Default coordinator" },
    ...(coordinators().length > 0 ? [{ value: "host" as Backend, label: "Coordinator" }] : []),
    ...(catalog()?.providers ?? []).map(group => ({ value: `cli:${group.provider}` as Backend, label: group.label })),
    ...(worker() ? [{ value: "lash" as Backend, label: worker()!.label }] : []),
  ]);
  // The coordinator's model question in whichever shape its provider takes —
  // the Settings main-agent view, asked of the draft instead of the store.
  const coordinator = createMemo(() => agentModelView("main", draft().providerId, draft().providerId, { id: draft().model, variant: draft().variant || "default" }, [], false));
  const group = () => {
    const backend = draft().backend;
    return backend.startsWith("cli:") ? catalog()?.providers.find(entry => entry.provider === backend.slice(4)) : undefined;
  };
  const available = createMemo<AvailableModel[]>(() => (group()?.models ?? [])
    .filter(model => model.enabled && model.enabled_variants.length > 0)
    .map(model => ({ id: model.id, label: model.label, variants: model.enabled_variants, default_variant: model.enabled_variants[0] })));
  const current = () => {
    const first = available()[0];
    const chosen = available().find(model => model.id === draft().model) ?? first;
    return { id: chosen?.id ?? draft().model, variant: chosen && chosen.variants.includes(draft().variant) ? draft().variant : chosen?.default_variant ?? draft().variant };
  };
  const target = (): ThreadExecutionTarget | null => {
    const { backend, providerId } = draft();
    if (backend === "default") return null;
    if (backend === "host") return { kind: "host", provider_id: providerId, model: coordinator().current.id.trim() };
    if (backend === "lash") return { kind: "lash", provider_id: providerId, model: draft().model.trim(), variant: "default" };
    return { kind: "cli", agent: backend.slice(4), model: current().id, variant: current().variant };
  };
  const ready = () => {
    const chosen = target();
    if (!chosen) return true;
    if (chosen.kind === "host" || chosen.kind === "lash") return Boolean(chosen.provider_id && chosen.model);
    return Boolean(chosen.model && chosen.variant);
  };
  const open = () => { setDraft(initial()); edit.setEditing(true); };
  const summary = () => executionLabel(props.thread.execution);
  return <Show when={edit.editing()} fallback={<div class="flex min-w-0 flex-wrap items-center gap-0.5">
    <span class={`min-w-0 ${summary().muted ? "text-muted-foreground" : ""}`} data-slot="thread-execution">{summary().text}</span>
    <button type="button" class={`${quiet} shrink-0`} aria-label="Change where this Thread runs" title="Change where this Thread runs" onClick={open}><SquarePen class="size-3.5" /></button>
  </div>}>
    <div class="flex min-w-0 flex-col gap-1" data-slot="thread-execution-editor">
      {/* The `dt` beside this editor already says "Runs on"; a second label
          inside it only repeats the row's own name. */}
      <div class="py-1">
        <Select ariaLabel="Where this Thread runs" class="w-full" value={draft().backend}
          options={options()} onChange={value => {
            const backend = value as Backend;
            const providerId = defaultProviderId(backend);
            setDraft({ backend, providerId, model: backend === "host" ? coordinatorSeedModel(providerId) : "", variant: "" });
          }} />
      </div>
      {/* A select with one choice is a dead control unless it says why it is
          alone: the other backends are the CLI agents this host can see and the
          native worker, both configured in Settings, not here. */}
      <Show when={options().length === 1}>
        <p class="text-meta text-muted-foreground">No other backend is available. The coordinator appears here once a provider is configured, the Claude and Codex CLI agents once this host can see them, and the native worker once it is turned on in Settings.</p>
      </Show>
      <Show when={draft().backend === "host"}>
        <div class="flex items-center justify-between gap-3 py-1">
          <span class="text-sm">Provider</span>
          <Select ariaLabel="Coordinator provider" class="w-[10.5rem] shrink-0" value={draft().providerId}
            options={coordinators().map(instance => ({ value: instance.id, label: instance.label }))}
            onChange={providerId => setDraft(previous => ({ ...previous, providerId, model: coordinatorSeedModel(providerId), variant: "" }))} />
        </div>
      </Show>
      <Show when={draft().backend === "lash" && worker()}>{native => <div class="flex items-center justify-between gap-3 py-1">
        <span class="text-sm">Provider</span>
        <Select ariaLabel="Native worker provider" class="w-[10.5rem] shrink-0" value={draft().providerId}
          options={native().eligible_provider_ids.map(id => ({ value: id, label: providerLabel(id) || id }))}
          onChange={providerId => setDraft(previous => ({ ...previous, providerId }))} />
      </div>}</Show>
      <Show when={draft().backend === "host"}>
        <div class="divide-y divide-border">
          <AgentModelRows name="This Thread" freeText={coordinator().freeText} current={coordinator().current} available={coordinator().available}
            placeholder={coordinator().placeholder} pending={pending} modelKey="thread-model" variantKey="thread-variant"
            onSelect={selection => { setDraft(previous => ({ ...previous, model: selection.id, variant: selection.variant })); pending.settleAll(); }}
            onFreeText={model => { setDraft(previous => ({ ...previous, model })); pending.settleAll(); }} />
        </div>
      </Show>
      <Show when={draft().backend !== "default" && draft().backend !== "host"}>
        <div class="divide-y divide-border">
          <AgentModelRows name="This Thread" freeText={draft().backend === "lash"} current={draft().backend === "lash" ? { id: draft().model, variant: "default" } : current()}
            available={draft().backend === "lash" ? [] : available()} placeholder={worker()?.model} pending={pending} modelKey="thread-model" variantKey="thread-variant"
            onSelect={selection => { setDraft(previous => ({ ...previous, model: selection.id, variant: selection.variant })); pending.settleAll(); }}
            onFreeText={model => { setDraft(previous => ({ ...previous, model })); pending.settleAll(); }} />
        </div>
      </Show>
      <p class="text-meta text-muted-foreground">Applies from this Thread's next turn. A running turn keeps the backend it started on.</p>
      <div class="flex items-center gap-1.5">
        <button type="button" class={primary} aria-label="Save where this Thread runs" disabled={edit.saving() || !ready()} onClick={() => edit.submit("set_execution", { execution: target() })}>Save</button>
        <button type="button" class={action} aria-label="Cancel execution change" disabled={edit.saving()} onClick={() => edit.setEditing(false)}>Cancel</button>
      </div>
      <Failure threadId={props.thread.id} />
    </div>
  </Show>;
}

/** The Thread itself, as a pane inside its own frame: what it is, what it is
 * for, and the few facts the client actually holds about it. */
export function ThreadInfo(props: { thread: Thread; historyId: string; onRelated: () => void }) {
  const [now, setNow] = createSignal(Date.now());
  const timer = setInterval(() => setNow(Date.now()), 30_000);
  onCleanup(() => clearInterval(timer));
  const history = () => threadState.histories[props.thread.id];
  const children = createMemo(() => threadState.threads.filter(thread => thread.parent_thread_id === props.thread.id));
  const parent = () => props.thread.parent_thread_id;
  const brief = () => history()?.brief;
  const related = () => relatedState.lists[props.thread.id]?.items.length ?? 0;
  const processes = createMemo(() => state.processes.filter(process => process.thread_id === props.thread.id).length);
  return <div class="mx-auto flex w-full max-w-measure flex-col gap-5" data-slot="thread-info">
    <div class="flex items-start gap-3">
      <button type="button" class="shrink-0 rounded-xl focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" aria-label="Change thread icon" title="Change thread icon" onClick={() => openThreadIconPicker(props.thread)}>
        <ThreadAvatar thread={props.thread} large />
      </button>
      <div class="flex min-w-0 flex-1 flex-col gap-1">
        <TitleRow thread={props.thread} historyId={props.historyId} />
        <span class="text-meta capitalize text-muted-foreground">{props.thread.kind === "space" ? "Space" : "Task"}{props.thread.kind === "task" && props.thread.settled_at ? " · Done" : ""}</span>
      </div>
    </div>
    <DescriptionSection thread={props.thread} historyId={props.historyId} />
    <dl class="divide-y divide-border/60 border-t border-border/60">
      <Fact label="Runs on"><RunsOnRow thread={props.thread} historyId={props.historyId} /></Fact>
      <Fact label="Parent"><Show when={parent() !== null} fallback={<span class="text-muted-foreground">Top level</span>}><ThreadLink id={parent()!} /></Show></Fact>
      <Show when={children().length > 0}><Fact label="Children">
        <ul class="flex flex-col gap-1"><For each={children()}>{child => <li class="flex flex-wrap items-center gap-2"><ThreadLink id={child.id} /><ThreadStatus thread={child} now={now()} /></li>}</For></ul>
      </Fact></Show>
      <Fact label="Created"><span title={absolute(props.thread.created_at)}>{formatRelativeTime(props.thread.created_at, now())}</span></Fact>
      <Fact label="Updated"><span title={absolute(props.thread.updated_at)}>{formatRelativeTime(props.thread.updated_at, now())}</span></Fact>
      <Show when={brief()?.text || brief()?.artifact_ids.length}><Fact label="Current brief">
        <div class="space-y-2"><Markdown>{brief()?.text ?? ""}</Markdown><For each={brief()?.artifact_ids ?? []}>{id => <ArtifactCard id={id} />}</For></div>
      </Fact></Show>
      <Show when={related() > 0}><Fact label="Related">
        <button type="button" class="underline decoration-dotted underline-offset-2 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" onClick={props.onRelated}>{related()} {related() === 1 ? "link" : "links"}</button>
      </Fact></Show>
      <Show when={processes() > 0}><Fact label="Processes">
        <button type="button" class="underline decoration-dotted underline-offset-2 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" onClick={() => openProcesses()}>{processes()} on this Thread</button>
      </Fact></Show>
    </dl>
  </div>;
}
