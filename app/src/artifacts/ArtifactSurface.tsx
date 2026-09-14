import { ArtifactOpenWith } from "./ArtifactOpenWith";
import { downloadArtifact } from "./download";
import { draftArtifact } from "./draft-context";
import { createEffect, onCleanup, For, Match, Show, Switch } from "solid-js";
import { ArrowDownToLine, ArrowUpRight, FileText, X } from "../components/ui/icons";
import { createMediaFlag, createOverlayPresence } from "../lib/focus";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "../components/ui/empty";
import { ArtifactPreview } from "./ArtifactPreview";
import { ArtifactPresentationToggle } from "./ArtifactPresentationMode";
import { artifactCaption, hasArtifactSource, renderModeFor } from "./render-mode";
import { previewArtifact, stagePreviewContext } from "./openers";
import { artifactState, closeArtifact, openArtifact, listArtifacts, inventoryError, openedArtifact, previewMode, previewedArtifactId, setPreviewMode } from "./store";
import { threadState } from "../threads/store";
import { ThreadLink } from "../threads/ThreadRef";
import { NeutralTile } from "../threads/ThreadAvatar";
import type { ArtifactSummary } from "./types";
const button = "inline-flex min-h-11 min-w-11 items-center gap-2 rounded-lg px-3 py-2 text-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring";
const OPEN_LABEL: Record<string, string> = { solid: "Open interactive artifact", html: "Open HTML artifact", markdown: "Open document", openui: "Open interactive artifact", image: "Open image", text: "Open file" };
/** The preview’s failure, when that is the variant it is in. */
function previewError(): { id: number; message: string } | null { return artifactState.preview.status === "error" ? artifactState.preview : null; }
export function ArtifactCard(props: { id: number }) {
  const artifact = () => artifactState.summaries.find(row => row.id === props.id);
  return <div data-slot="artifact-card" class="mt-3 flex w-full max-w-sm items-center rounded-lg border border-border"><button type="button" data-artifact-ref={props.id} title={(() => { const summary = artifact(); return summary ? OPEN_LABEL[renderModeFor(summary)] : "Open artifact"; })()} class={`${button} flex min-w-0 flex-1 items-center gap-3 text-left`} onClick={() => previewArtifact(props.id, artifact()?.title ?? `Artifact #${props.id}`)}>
    <FileText class="size-4 shrink-0" /><span class="min-w-0 flex-1 truncate font-medium">{artifact()?.title ?? `Artifact #${props.id}`}</span><ArrowUpRight class="size-4 shrink-0" />
  </button><ArtifactOpenWith artifactId={props.id} /></div>;
}
export function ArtifactList(props: { threadId?: number; embedded?: boolean; onResume?: () => void }) {
  const rows = () => artifactState.summaries.filter(row => props.threadId === undefined || row.thread_ids.includes(props.threadId));
  const idle = () => artifactState.inventory.status === "ready" && rows().length === 0;
  return <div data-slot="artifact-list" class="mx-auto w-full max-w-measure">
    {/* The global list is titled by its pane header; only the copy embedded
        in Related carries its own heading — and not when it has nothing to
        head: Related shows one empty state, its own, never two stacked. */}
    <Show when={props.embedded && !idle()}><h3 class="text-sm font-medium">Artifacts</h3></Show>
    <Switch>
      <Match when={artifactState.inventory.status === "loading"}><p role="status" class="mt-4 text-sm text-muted-foreground">Loading artifacts…</p></Match>
      <Match when={inventoryError()}>{message => <div role="alert" class="mt-4 space-y-2 text-sm"><p>Couldn’t load the artifact list. Your existing results are kept.</p><button class={button} onClick={listArtifacts}>Retry loading artifacts</button><p class="break-words text-muted-foreground">{message()}</p></div>}</Match>
      {/* The SAME empty state the Processes list uses — one component, so an empty
          inventory reads the same wherever the Owner meets one. */}
      <Match when={idle() && !props.embedded}>
        <Empty class="border-none">
          <EmptyHeader>
            <EmptyMedia variant="icon"><FileText class="size-5" /></EmptyMedia>
            <EmptyTitle>No artifacts</EmptyTitle>
            <EmptyDescription>Artifacts Hirsel creates and shares will appear here.</EmptyDescription>
          </EmptyHeader>
        </Empty>
      </Match>
    </Switch>
    <Show when={rows().length > 0}>
      {/* The same dense row the Thread inventory uses: tile, name, one meta
          word, the row's own menu — an artifact reads as a thing with an
          identity, not a paragraph with a button. Where it is cited is a row
          of Thread chips, the one way a Thread is named inline anywhere. */}
      <ul class="mt-3 flex flex-col gap-0.5"><For each={rows()}>{artifact => <li class="flex flex-col gap-0.5"><ArtifactRow artifact={artifact} />
        <Show when={props.threadId === undefined && artifact.thread_ids.length > 0}><p class="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-0.5 pl-[2.375rem] pr-2 text-xs text-muted-foreground"><span class="shrink-0">Referenced in</span><For each={artifact.thread_ids}>{id => <span class="min-w-0 text-foreground"><ThreadLink id={id} onOpen={props.onResume} /></span>}</For></p></Show>
      </li>}</For></ul>
    </Show>
  </div>;
}
const artifactRow = "flex min-h-11 w-full min-w-0 flex-1 items-center gap-2.5 rounded-md px-2 py-1 text-left text-sm transition-colors hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring split:min-h-9";
function ArtifactRow(props: { artifact: ArtifactSummary }) {
  return <div class="flex min-w-0 items-center gap-0.5"><button data-artifact-ref={props.artifact.id} class={artifactRow} title={OPEN_LABEL[renderModeFor(props.artifact)]} onClick={() => previewArtifact(props.artifact.id, props.artifact.title)}>
    <NeutralTile><FileText class="size-3" /></NeutralTile>
    <span class="min-w-0 flex-1 truncate font-medium">{props.artifact.title}</span>
    <span class="shrink-0 truncate text-meta text-muted-foreground">{artifactCaption(props.artifact)}</span>
  </button><ArtifactOpenWith artifactId={props.artifact.id} /></div>;
}
export function ArtifactSurface() {
  return <Show when={artifactState.preview.status !== "idle"}><ArtifactPanel /></Show>;
}
function ArtifactPanel() {
  let panel: HTMLDialogElement | undefined;
  const isPhone = createMediaFlag("(max-width: 1023px)");
  const restoreTarget = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const returnThread = threadState.focusedId;
  const returnArtifact = previewedArtifactId();
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
    <dialog ref={node => { panel = node; }} onCancel={dismiss} onKeyDown={event => { if (event.key === "Escape") { event.stopPropagation(); dismiss(event); } }} role={isPhone() ? "dialog" : "complementary"} aria-modal={isPhone() ? "true" : undefined} aria-label="Artifact preview" data-slot="artifact-preview" class="fixed inset-0 z-40 m-0 flex h-full max-h-none w-full max-w-none min-h-0 min-w-0 flex-col border-0 bg-background p-0 text-foreground outline-none lg:static lg:z-auto lg:w-[clamp(20rem,44%,42rem)] lg:shrink lg:border-l lg:border-border">
      <header class="flex shrink-0 flex-wrap items-center gap-2 border-b border-border/60 px-4 py-3">
        <h2 class="min-w-0 w-full truncate font-semibold sm:w-auto sm:flex-1">{openedArtifact()?.title ?? "Artifact"}</h2>
        <Show when={openedArtifact() && hasArtifactSource(openedArtifact()!)}><ArtifactPresentationToggle mode={previewMode()} onChange={setPreviewMode} /></Show>
        <Show when={openedArtifact()}><button class={button} onClick={() => downloadArtifact(openedArtifact()!)} title="Download" aria-label="Download"><ArrowDownToLine class="size-4" /><span class="hidden 2xl:inline">Download</span></button></Show>
        <button class={button} onClick={closeArtifact} title="Back to conversation" aria-label="Back to conversation"><X class="size-5" /></button>
      </header>
      <Show when={openedArtifact() && threadState.focusedId !== null && threadState.threads.some(thread => thread.id === threadState.focusedId) && draftArtifact(threadState.focusedId)?.id !== openedArtifact()!.id}><div class="flex shrink-0 items-center gap-2 border-b border-border/60 px-4"><button class={`${button} min-w-0 text-xs`} onClick={() => stagePreviewContext(openedArtifact()!.id, openedArtifact()!.title)}>Use in message</button><span class="min-w-0 truncate text-xs text-muted-foreground">#{threadState.focusedId} {threadState.threads.find(thread => thread.id === threadState.focusedId)?.title}</span></div></Show>
      <Switch>
      <Match when={previewError()}>{failure => <div role="alert" class="p-6 text-sm"><p>{failure().message}</p><button class={button} onClick={() => openArtifact(failure().id)}>Try again</button></div>}</Match>
      <Match when={artifactState.preview.status === "loading"}><p role="status" class="p-6 text-sm text-muted-foreground">Loading artifact…</p></Match>
      <Match when={openedArtifact()}>{artifact => <div class="min-h-0 flex-1 overflow-auto"><ArtifactPreview artifact={artifact()} mode={previewMode()} threadId={threadState.focusedId} onDismiss={closeArtifact} onReturnToComposer={() => { closeArtifact(); queueMicrotask(() => document.querySelector<HTMLElement>(`main[data-thread-id="${threadState.focusedId}"] [data-composer="main"]`)?.focus()); }} /></div>}</Match>
      </Switch>
    </dialog>
  </>;
}
