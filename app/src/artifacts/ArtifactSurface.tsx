import { ArtifactActions } from "./ShowcaseSurface";
import { downloadArtifact } from "./download";
import { draftArtifact, stageDraftArtifact } from "./draft-context";
import { createEffect, createSignal, onCleanup, For, Show } from "solid-js";
import { ArrowDownToLine, ArrowUpRight, FileText, X } from "../components/ui/icons";
import { createMediaFlag, createOverlayPresence } from "../lib/focus";
import { ArtifactPreview } from "./ArtifactPreview";
import { ArtifactPresentationToggle, hasArtifactPresentationModes, type ArtifactPresentationMode } from "./ArtifactPresentationMode";
import { artifactState, closeArtifact, openArtifact, listArtifacts } from "./store";
import { focusThread, threadState } from "../threads/store";
import type { ArtifactSummary } from "./types";
const button = "inline-flex min-h-11 min-w-11 items-center gap-2 rounded-lg px-3 py-2 text-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring";
function stagePreviewContext(id: number, title: string): void {
  const threadId = threadState.focusedId;
  if (threadId !== null && threadState.threads.some(thread => thread.id === threadId)) stageDraftArtifact(threadId, { id, title });
}
function previewArtifact(id: number, title: string): void { stagePreviewContext(id, title); openArtifact(id); }
export function ArtifactCard(props: { id: number }) {
  const artifact = () => artifactState.summaries.find(row => row.id === props.id);
  return <div class="mt-3 flex w-full max-w-sm items-center rounded-lg border border-border"><button type="button" data-artifact-ref={props.id} title={artifact()?.kind === "solid" ? "Open interactive artifact" : artifact()?.kind === "html" ? "Open HTML artifact" : "Open file"} class={`${button} flex min-w-0 flex-1 items-center gap-3 text-left`} onClick={() => previewArtifact(props.id, artifact()?.title ?? `Artifact #${props.id}`)}>
    <FileText class="size-4 shrink-0" /><span class="min-w-0 flex-1 truncate font-medium">{artifact()?.title ?? `Artifact #${props.id}`}</span><ArrowUpRight class="size-4 shrink-0" />
  </button><ArtifactActions artifactId={props.id} /></div>;
}
export function ArtifactList(props: { threadId?: number; onResume?: () => void; embedded?: boolean }) {
  const rows = () => artifactState.summaries.filter(row => props.threadId === undefined || row.thread_ids.includes(props.threadId));
  const resume = (id: number) => { props.onResume?.(); closeArtifact(); focusThread(id); };
  return <div data-slot="artifact-list" class="mx-auto w-full max-w-measure">
    <Show when={props.embedded} fallback={<h2 class="text-lg font-medium">{props.threadId === undefined ? "All artifacts" : "Artifacts"}</h2>}><h3 class="text-sm font-medium">Artifacts</h3></Show>
    <Show when={artifactState.listing}><p role="status" class="mt-4 text-sm text-muted-foreground">Loading artifacts…</p></Show>
    <Show when={artifactState.listError}><div role="alert" class="mt-4 space-y-2 text-sm"><p>Couldn’t load the artifact list. Your existing results are kept.</p><button class={button} onClick={listArtifacts}>Retry loading artifacts</button><details><summary class="cursor-pointer py-2 text-muted-foreground">Technical details</summary><p class="break-words">{artifactState.listError}</p></details></div></Show>
    <Show when={!artifactState.listing && !artifactState.listError && artifactState.listed && rows().length === 0}><p class="mt-4 max-w-prose text-sm leading-relaxed text-muted-foreground">Artifacts Hirsel creates and shares will appear here.</p></Show>
    <Show when={rows().length > 0}>
      <ul class="mt-4 divide-y divide-border"><For each={rows()}>{artifact => <li class="py-3"><ArtifactRow artifact={artifact} />
        <Show when={props.threadId === undefined && artifact.thread_ids.length > 0}><div class="mt-1"><p class="px-3 text-xs text-muted-foreground">Referenced in</p><div class="flex flex-wrap gap-1"><For each={artifact.thread_ids}>{id => <button class={button} onClick={() => resume(id)}>{threadState.threads.find(row => row.id === id)?.title ?? `Thread #${id}`}</button>}</For></div></div></Show>
      </li>}</For></ul>
    </Show>
  </div>;
}
function ArtifactRow(props: { artifact: ArtifactSummary }) {
  return <div class="flex items-center"><button data-artifact-ref={props.artifact.id} class={`${button} flex min-w-0 flex-1 items-center justify-between gap-3 text-left`} onClick={() => previewArtifact(props.artifact.id, props.artifact.title)}><span class="min-w-0"><span class="block break-words font-medium">{props.artifact.title}</span><span class="text-xs text-muted-foreground">{props.artifact.filename ?? props.artifact.kind}</span></span><span class="text-xs">Open</span></button><ArtifactActions artifactId={props.artifact.id} /></div>;
}
export function ArtifactSurface() {
  return <Show when={artifactState.selectedId !== null}><ArtifactPanel /></Show>;
}
function ArtifactPanel() {
  let panel: HTMLDialogElement | undefined;
  const [mode, setMode] = createSignal<ArtifactPresentationMode>("rendered");
  const isPhone = createMediaFlag("(max-width: 1023px)");
  const restoreTarget = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const returnThread = threadState.focusedId;
  const returnArtifact = artifactState.selectedId;
  createOverlayPresence(() => true);
  createEffect(() => artifactState.selectedId, (id, previous) => {
    if (previous !== undefined && id !== previous) setMode("rendered");
  });
  createEffect(isPhone, phone => {
    if (!panel) return;
    // Native modal navigation includes the opaque iframe's focusable content.
    // Keep the same preview mounted when the viewport changes.
    panel.close();
    if (phone) panel.showModal();
    else panel.show();
  });
  onCleanup(() => {
    panel?.close();
    const conversation = document.querySelector<HTMLElement>(`main[data-thread-id="${threadState.focusedId}"]`);
    const sameThread = returnThread === threadState.focusedId;
    const destination = sameThread && restoreTarget?.isConnected ? restoreTarget
      : (sameThread ? conversation?.querySelector<HTMLElement>(`[data-artifact-ref="${returnArtifact}"]`) : null)
        ?? conversation?.querySelector<HTMLElement>('[data-composer="main"]');
    destination?.focus();
  });
  const dismiss = (event: Event) => { event.preventDefault(); closeArtifact(); };
  return <>
    <dialog ref={node => { panel = node; }} onCancel={dismiss} onKeyDown={event => { if (event.key === "Escape") { event.stopPropagation(); dismiss(event); } }} role={isPhone() ? "dialog" : "complementary"} aria-modal={isPhone() ? "true" : undefined} aria-label="Artifact preview" data-slot="artifact-preview" class="fixed inset-0 z-40 m-0 flex h-full max-h-none w-full max-w-none min-h-0 min-w-0 flex-col border-0 bg-background p-0 text-foreground outline-none lg:static lg:z-auto lg:w-[44%] lg:min-w-80 lg:border-l lg:border-border">
      <header class="flex shrink-0 flex-wrap items-center gap-2 border-b border-border/60 px-4 py-3">
        <h2 class="min-w-0 w-full truncate font-semibold sm:w-auto sm:flex-1">{artifactState.opened?.title ?? "Artifact"}</h2>
        <Show when={artifactState.opened && hasArtifactPresentationModes(artifactState.opened)}><ArtifactPresentationToggle mode={mode()} onChange={setMode} /></Show>
        <Show when={artifactState.opened}><button class={button} onClick={() => downloadArtifact(artifactState.opened!)} title="Download" aria-label="Download"><ArrowDownToLine class="size-4" /><span class="hidden xl:inline">Download</span></button></Show>
        <button class={button} onClick={closeArtifact} title="Back to conversation" aria-label="Back to conversation"><X class="size-5" /></button>
      </header>
      <Show when={artifactState.opened && threadState.focusedId !== null && threadState.threads.some(thread => thread.id === threadState.focusedId) && draftArtifact(threadState.focusedId)?.id !== artifactState.opened.id}><div class="flex shrink-0 items-center gap-2 border-b border-border/60 px-4"><button class={`${button} min-w-0 text-xs`} onClick={() => stagePreviewContext(artifactState.opened!.id, artifactState.opened!.title)}>Use in message</button><span class="min-w-0 truncate text-xs text-muted-foreground">#{threadState.focusedId} {threadState.threads.find(thread => thread.id === threadState.focusedId)?.title}</span></div></Show>
      <Show when={artifactState.error}><div role="alert" class="p-6 text-sm"><p>{artifactState.error}</p><button class={button} onClick={() => openArtifact(artifactState.selectedId!)}>Try again</button></div></Show>
      <Show when={artifactState.loading}><p role="status" class="p-6 text-sm text-muted-foreground">Loading artifact…</p></Show>
      <Show when={artifactState.opened}>{artifact => <div class="min-h-0 flex-1 overflow-auto"><ArtifactPreview artifact={artifact()} mode={mode()} onDismiss={closeArtifact} onReturnToComposer={() => { closeArtifact(); queueMicrotask(() => document.querySelector<HTMLElement>(`main[data-thread-id="${threadState.focusedId}"] [data-composer="main"]`)?.focus()); }} /></div>}</Show>
    </dialog>
  </>;
}
