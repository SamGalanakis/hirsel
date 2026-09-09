import { createEffect, onCleanup, For, Show } from "solid-js";
import { ArrowDownToLine, ArrowUpRight, FileText, X } from "../components/ui/icons";
import { createMediaFlag, createOverlayPresence } from "../lib/focus";
import { ArtifactPreview } from "./ArtifactPreview";
import { artifactState, closeArtifact, openArtifact, listArtifacts } from "./store";
import { focusThread, threadState } from "../threads/store";
import type { ArtifactSummary } from "./types";
const button = "inline-flex min-h-11 min-w-11 items-center gap-2 rounded-lg px-3 py-2 text-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring";
export function ArtifactCard(props: { id: number }) {
  const artifact = () => artifactState.summaries.find(row => row.id === props.id);
  return <button type="button" data-artifact-ref={props.id} title={artifact()?.kind === "solid" ? "Open interactive artifact" : artifact()?.kind === "html" ? "Open HTML artifact" : "Open file"} class={`${button} mt-3 flex w-full max-w-sm items-center gap-3 border border-border text-left`} onClick={() => openArtifact(props.id)}>
    <FileText class="size-4 shrink-0" /><span class="min-w-0 flex-1 truncate font-medium">{artifact()?.title ?? `Artifact #${props.id}`}</span><ArrowUpRight class="size-4 shrink-0" />
  </button>;
}
export function ArtifactList(props: { threadId?: number; onResume?: () => void }) {
  const rows = () => artifactState.summaries.filter(row => props.threadId === undefined || row.thread_ids.includes(props.threadId));
  const resume = (id: number) => { props.onResume?.(); closeArtifact(); focusThread(id); };
  return <div data-slot="artifact-list" class="mx-auto w-full max-w-measure">
    <h2 class="text-lg font-medium">{props.threadId === undefined ? "All artifacts" : "Artifacts"}</h2>
    <Show when={artifactState.listing}><p role="status" class="mt-4 text-sm text-muted-foreground">Loading artifacts…</p></Show>
    <Show when={artifactState.listError}><div role="alert" class="mt-4 space-y-2 text-sm"><p>Couldn’t load the artifact list. Your existing results are kept.</p><button class={button} onClick={listArtifacts}>Retry loading artifacts</button><details><summary class="cursor-pointer py-2 text-muted-foreground">Technical details</summary><p class="break-words">{artifactState.listError}</p></details></div></Show>
    <Show when={!artifactState.listing && !artifactState.listError && artifactState.listed && rows().length === 0}><p class="mt-4 max-w-prose text-sm leading-relaxed text-muted-foreground">Artifacts Hirsel creates and shares will appear here.</p></Show>
    <Show when={rows().length > 0}>
      <ul class="mt-4 divide-y divide-border"><For each={rows()}>{artifact => <li class="py-3"><ArtifactRow artifact={artifact} />
        <Show when={props.threadId === undefined && artifact.thread_ids.length > 0}><div class="flex flex-wrap gap-1"><For each={artifact.thread_ids}>{id => <button class={button} onClick={() => resume(id)}>{id === 0 ? "Home" : threadState.threads.find(row => row.id === id)?.title ?? `Thread #${id}`}</button>}</For></div></Show>
      </li>}</For></ul>
    </Show>
  </div>;
}
function ArtifactRow(props: { artifact: ArtifactSummary }) {
  return <button data-artifact-ref={props.artifact.id} class={`${button} flex w-full items-center justify-between gap-3 text-left`} onClick={() => openArtifact(props.artifact.id)}><span class="min-w-0"><span class="block break-words font-medium">{props.artifact.title}</span><span class="text-xs text-muted-foreground">{props.artifact.filename ?? props.artifact.kind}</span></span><span class="text-xs">Open</span></button>;
}
function download() {
  const artifact = artifactState.opened;
  if (!artifact) return;
  const url = URL.createObjectURL(new Blob([artifact.content], { type: artifact.mime }));
  const link = document.createElement("a");
  link.href = url; link.download = artifact.filename ?? `${artifact.title}.${artifact.kind === "solid" ? "jsx" : artifact.kind === "html" ? "html" : "txt"}`; link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
export function ArtifactSurface() {
  return <Show when={artifactState.selectedId !== null}><ArtifactPanel /></Show>;
}
function ArtifactPanel() {
  let panel: HTMLDialogElement | undefined;
  const isPhone = createMediaFlag("(max-width: 1023px)");
  const restoreTarget = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const returnThread = threadState.focusedId;
  const returnArtifact = artifactState.selectedId;
  createOverlayPresence(() => true);
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
      <header class="flex shrink-0 items-center gap-2 border-b border-border/60 px-4 py-3">
        <h2 class="min-w-0 flex-1 truncate font-semibold">{artifactState.opened?.title ?? "Artifact"}</h2>
        <Show when={artifactState.opened}><button class={button} onClick={download} title="Download" aria-label="Download"><ArrowDownToLine class="size-4" /><span class="hidden xl:inline">Download</span></button></Show>
        <button class={button} onClick={closeArtifact} title="Back to conversation" aria-label="Back to conversation"><X class="size-5" /></button>
      </header>
      <Show when={artifactState.error}><div role="alert" class="p-6 text-sm"><p>{artifactState.error}</p><button class={button} onClick={() => openArtifact(artifactState.selectedId!)}>Try again</button></div></Show>
      <Show when={artifactState.loading}><p role="status" class="p-6 text-sm text-muted-foreground">Loading artifact…</p></Show>
      <Show when={artifactState.opened}>{artifact => <div class="min-h-0 flex-1 overflow-auto"><ArtifactPreview artifact={artifact()} onDismiss={closeArtifact} onReturnToComposer={() => { closeArtifact(); queueMicrotask(() => document.querySelector<HTMLElement>(`main[data-thread-id="${threadState.focusedId}"] [data-composer="main"]`)?.focus()); }} /></div>}</Show>
    </dialog>
  </>;
}
