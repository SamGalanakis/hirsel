import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { MoreHorizontal, ArrowDownToLine, PanelRight, X } from "../components/ui/icons";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "../components/ui/dropdown-menu";
import { createMediaFlag, createOverlayPresence } from "../lib/focus";
import { historyId } from "../lib/history";
import { state } from "../store/store";
import { ThreadError } from "../threads/ThreadError";
import { threadState } from "../threads/store";
import { useRelatedOrigin } from "../related/context";
import type { RelatedOrigin } from "../related/store";
import { ArtifactPreview } from "./ArtifactPreview";
import { downloadArtifact } from "./download";
import { artifactState, listArtifacts } from "./store";
import { captureShowcaseOrigin, phoneShowcase, setPhoneShowcase, setShowcasePicker, setThreadShowcase, showcasePicker, type ShowcaseOrigin } from "./showcase-actions";
import { refreshShowcase, selectShowcase, showcaseState } from "./showcase-store";

const button = "inline-flex min-h-11 min-w-11 items-center justify-center gap-2 rounded-lg px-3 text-sm text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-40";
export function ArtifactActions(props: { artifactId: number }) {
  const origin = useRelatedOrigin();
  let target: ShowcaseOrigin | null = null;
  const [error, setError] = createSignal<string | null>(null);
  return <Show when={origin}><div class="shrink-0"><DropdownMenu onOpenChange={open => { if (open) { target = captureShowcaseOrigin(origin); setError(null); } }}>
    <DropdownMenuTrigger class={button} aria-label="Artifact actions"><MoreHorizontal class="size-4" /></DropdownMenuTrigger>
    <DropdownMenuContent><DropdownMenuItem onSelect={() => {
      try { if (!target) throw new Error("Reconnect and reopen this menu to choose a showcase."); setThreadShowcase(target, props.artifactId); }
      catch (error) { setError(error instanceof Error ? error.message : "Couldn’t showcase this artifact."); }
    }}>Showcase in this thread</DropdownMenuItem></DropdownMenuContent>
  </DropdownMenu><Show when={error()}><p role="alert" class="max-w-60 px-2 text-xs text-status-danger">{error()}</p></Show></div></Show>;
}
export function ShowcaseButton(props: { threadId: number }) {
  const current = () => threadState.threads.find(thread => thread.id === props.threadId);
  return <Show when={current()?.showcased_artifact_id != null}><button class={`${button} lg:hidden`} aria-label="Show showcase" title="Show showcase" onClick={() => { const history = historyId(); if (history) setPhoneShowcase({ historyId: history, threadId: props.threadId }); }}><PanelRight class="size-4" /></button></Show>;
}
export function ShowcaseSurface() {
  const originKey = createMemo(() => {
    const history = historyId(); const id = threadState.focusedId;
    return history && id !== null ? JSON.stringify([history, id]) : null;
  });
  return <><Show when={originKey()} keyed>{key => {
    const [history, threadId] = JSON.parse(key) as [string, number];
    return <ThreadShowcase origin={{ historyId: history, threadId }} />;
  }}</Show><ShowcasePicker /></>;
}
function ThreadShowcase(props: { origin: RelatedOrigin }) {
  const current = () => threadState.threads.find(thread => thread.id === props.origin.threadId);
  const phone = createMediaFlag("(max-width: 1023px)");
  const visible = () => current()?.showcased_artifact_id != null && artifactState.selectedId === null && (!phone() || (phoneShowcase()?.historyId === props.origin.historyId && phoneShowcase()?.threadId === props.origin.threadId));
  let dialog: HTMLDialogElement | undefined;
  let restore: HTMLElement | null = null;
  let target: ShowcaseOrigin | null = null;
  const [error, setError] = createSignal<string | null>(null);
  createOverlayPresence(() => phone() && visible());
  createEffect(() => current()?.showcased_artifact_id ?? null, id => selectShowcase(props.origin.historyId, props.origin.threadId, id));
  createEffect(() => ({ visible: visible(), phone: phone() }), next => {
    if (!dialog) return;
    const active = document.activeElement;
    if (next.visible && !dialog.open) restore = active instanceof HTMLElement ? active : null;
    dialog.close();
    if (next.visible) {
      if (next.phone) dialog.showModal();
      else { dialog.show(); if (active instanceof HTMLElement && active.isConnected) active.focus(); }
    } else if (restore?.isConnected && active instanceof Node && dialog.contains(active)) restore.focus();
  });
  onCleanup(() => { dialog?.close(); setPhoneShowcase(null); });
  const close = () => { setPhoneShowcase(null); if (restore?.isConnected) restore.focus(); };
  const dismiss = (event: Event) => { event.preventDefault(); if (phone()) close(); };
  const replace = () => { if (target) setShowcasePicker(target); else setError("Reconnect to change the showcase."); };
  const remove = () => {
    try { if (!target) throw new Error("Reconnect to change the showcase."); setThreadShowcase(target, null); setError(null); }
    catch (error) { setError(error instanceof Error ? error.message : "Couldn’t remove the showcase."); }
  };
  return <dialog ref={node => { dialog = node; }} onCancel={dismiss} onKeyDown={event => { if (event.key === "Escape") { event.stopPropagation(); dismiss(event); } }} aria-label="Thread showcase" aria-modal={phone() ? "true" : undefined} role={phone() ? "dialog" : "complementary"} data-slot="thread-showcase" class="fixed inset-0 z-40 m-0 h-full max-h-none w-full max-w-none min-h-0 min-w-0 flex-col border-0 bg-background p-0 text-foreground outline-none open:flex lg:static lg:z-auto lg:w-[44%] lg:min-w-80 lg:border-l lg:border-border">
    <header class="flex shrink-0 items-center gap-1 border-b border-border/60 px-4 py-3"><div class="min-w-0 flex-1"><p class="text-xs text-muted-foreground">Showcase</p><h2 class="truncate font-semibold">{showcaseState.artifact?.title ?? "Artifact"}</h2></div>
      <Show when={showcaseState.artifact}><button class={button} title="Download showcase" aria-label="Download showcase" onClick={() => downloadArtifact(showcaseState.artifact!)}><ArrowDownToLine class="size-4" /></button></Show>
      <DropdownMenu onOpenChange={open => { if (open) { target = captureShowcaseOrigin(props.origin); setError(null); } }}><DropdownMenuTrigger class={button} aria-label="Showcase actions"><MoreHorizontal class="size-4" /></DropdownMenuTrigger><DropdownMenuContent><DropdownMenuItem onSelect={replace}>Replace showcase</DropdownMenuItem><DropdownMenuItem onSelect={remove}>Remove showcase</DropdownMenuItem></DropdownMenuContent></DropdownMenu>
      <button class={`${button} lg:hidden`} aria-label="Back to conversation" onClick={close}><X class="size-5" /></button>
    </header>
    <Show when={phone()}><ThreadError threadId={props.origin.threadId} /></Show>
    <Show when={error() || showcaseState.error}><div role="alert" class="p-4 text-sm"><p>{error() ?? showcaseState.error}</p><Show when={showcaseState.error}><button class={button} onClick={refreshShowcase}>Retry showcase</button></Show></div></Show>
    <Show when={showcaseState.loading}><p role="status" class="px-4 py-2 text-sm text-muted-foreground">Loading showcase…</p></Show>
    <Show when={showcaseState.artifact}>{artifact => <div class="min-h-0 flex-1 overflow-auto"><ArtifactPreview artifact={artifact()} onDismiss={() => { if (phone()) close(); }} onReturnToComposer={() => { close(); queueMicrotask(() => document.querySelector<HTMLElement>(`main[data-thread-id="${props.origin.threadId}"] [data-composer="main"]`)?.focus()); }} /></div>}</Show>
  </dialog>;
}
function ShowcasePicker() {
  let dialog: HTMLDialogElement | undefined;
  let restore: HTMLElement | null = null;
  const [error, setError] = createSignal<string | null>(null);
  const [search, setSearch] = createSignal("");
  const close = () => setShowcasePicker(null);
  createOverlayPresence(() => showcasePicker() !== null);
  createEffect(() => ({ target: showcasePicker(), history: historyId() }), ({ target, history }) => {
    if (target && target.historyId !== history) { close(); return; }
    if (target) {
      restore = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      setError(null); setSearch(""); listArtifacts(); dialog?.showModal();
    } else if (dialog?.open) { dialog.close(); if (restore?.isConnected) restore.focus(); }
  });
  onCleanup(() => { dialog?.close(); close(); });
  const choose = (id: number) => {
    try { const target = showcasePicker(); if (!target) return; setThreadShowcase(target, id); close(); }
    catch (error) { setError(error instanceof Error ? error.message : "Couldn’t change the showcase."); }
  };
  return <dialog ref={node => { dialog = node; }} aria-label="Choose showcase" onCancel={event => { event.preventDefault(); close(); }} class="m-auto max-h-[calc(100dvh-2rem)] w-[min(30rem,calc(100vw-2rem))] overflow-y-auto rounded-xl border border-border bg-background p-5 text-foreground backdrop:bg-black/30">
    <header class="flex items-start gap-2"><div class="min-w-0 flex-1"><h2 class="font-semibold">Choose showcase</h2><p class="mt-1 truncate text-sm text-muted-foreground">{showcasePicker()?.title}</p></div><button class={button} aria-label="Cancel showcase selection" onClick={close}><X class="size-4" /></button></header>
    <label class="mt-4 block text-sm">Find an artifact<input autofocus type="search" value={search()} onInput={event => setSearch(event.currentTarget.value)} class="mt-2 min-h-11 w-full rounded-lg border border-border bg-transparent px-3 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" /></label>
    <Show when={error() || artifactState.listError}><p role="alert" class="mt-3 text-sm text-status-danger">{error() ?? artifactState.listError}</p><Show when={artifactState.listError}><button class={button} onClick={listArtifacts}>Retry loading artifacts</button></Show></Show>
    <Show when={artifactState.listing}><p role="status" class="py-3 text-sm text-muted-foreground">Loading artifacts…</p></Show>
    <ul class="mt-3 divide-y divide-border"><For each={artifactState.summaries.filter(artifact => `${artifact.title} ${artifact.filename ?? ""}`.toLowerCase().includes(search().toLowerCase()))} fallback={<li class="py-4 text-sm text-muted-foreground">{artifactState.listing ? "" : "No matching artifacts."}</li>}>{artifact => <li><button class={`${button} w-full justify-start py-3 text-left`} aria-label={`Choose ${artifact.title} as showcase`} data-artifact-id={artifact.id} disabled={state.connection !== "connected"} onClick={() => choose(artifact.id)}><span class="min-w-0"><span class="block break-words font-medium text-foreground">{artifact.title}</span><span class="text-xs">{artifact.filename ?? artifact.kind}</span></span></button></li>}</For></ul>
  </dialog>;
}
