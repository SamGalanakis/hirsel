import { ThreadAvatar } from "../threads/ThreadAvatar";
import { createSignal, For, onSettled, Show } from "solid-js";
import { ArtifactList } from "../artifacts/ArtifactSurface";
import { ArrowUpRight, Copy, MoreHorizontal, Plus, X } from "../components/ui/icons";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "../components/ui/dropdown-menu";
import { historyId } from "../lib/history";
import { filterThreadCandidates } from "../lib/thread-ref";
import { parseThreadLink, plainPrimaryClick, threadReference, threadUrl } from "../lib/thread-url";
import { focusThread, threadState } from "../threads/store";
import { toast } from "../lib/toast";
import type { ThreadRelatedItem, RelatedTarget } from "../threads/types";
import { LinkIcon } from "./LinkIcon";
import { copyLink } from "./RichLink";
import { addRelatedItem, hasRelatedTarget, loadRelated, relatedState, removeRelatedItem, type RelatedOrigin } from "./store";
import { webLink } from "./url";
const control = "inline-flex min-h-11 items-center justify-center gap-2 rounded-lg px-3 text-sm text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50";
const input = "min-h-11 w-full min-w-0 rounded-lg border border-input bg-surface px-3 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring";
function SavedReference(props: { item: ThreadRelatedItem; origin: RelatedOrigin }) {
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const target = () => props.item.target;
  const localThread = () => { const ref = target(); return threadState.ready && ref.kind === "thread" && ref.history_id === historyId() ? threadState.threads.find(thread => thread.id === ref.thread_id) : undefined; };
  const web = () => { const ref = target(); return ref.kind === "url" ? webLink(ref.url) : null; };
  const href = () => { const ref = target(); return ref.kind === "thread" ? threadUrl(ref) : ref.url; };
  const title = () => { const ref = target(); return ref.kind === "thread" ? localThread()?.title ?? `Thread #${ref.thread_id} · unavailable` : props.item.title ?? web()?.label ?? ref.url; };
  const caption = () => { const ref = target(); return ref.kind === "thread" ? `Thread #${ref.thread_id}` : ref.url; };
  const usable = () => Boolean(localThread() || web());
  const copied = (value: string) => { setError(null); void copyLink(value).catch(failure => setError(failure.message)); };
  async function remove() {
    setBusy(true); setError(null);
    try { await removeRelatedItem(props.origin, props.item.id); toast(`Reference removed from Thread #${props.origin.threadId}`); }
    catch (failure) { setError(failure instanceof Error ? failure.message : String(failure)); }
    finally { setBusy(false); }
  }
  return <li class="py-2" data-related-item={props.item.id}>
    <div class="flex items-center gap-2"><Show when={localThread()} fallback={<LinkIcon kind={target().kind === "thread" ? "thread" : web()?.kind ?? "web"} class="size-4 shrink-0 text-muted-foreground" />}>{thread => <ThreadAvatar thread={thread()} small />}</Show>
      <Show when={usable()} fallback={<span class="min-w-0 flex-1 break-words text-sm">{title()}</span>}><a href={href()} target="_blank" rel="noopener noreferrer nofollow" title={href()} onClick={event => { if (localThread() && plainPrimaryClick(event)) { event.preventDefault(); focusThread(localThread()!.id); } }} class="min-w-0 flex-1 rounded-lg py-2 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"><span class="block break-words text-sm font-medium hover:underline">{title()}</span><span class="block truncate text-xs text-muted-foreground">{caption()}</span></a></Show>
      <DropdownMenu><DropdownMenuTrigger class={`${control} w-11 shrink-0 px-0`} aria-label={`Actions for ${title()}`} title="Reference actions" disabled={busy()}><MoreHorizontal class="size-4" /></DropdownMenuTrigger><DropdownMenuContent>
        <Show when={usable()}><DropdownMenuItem class="min-h-11" onSelect={() => window.open(href(), "_blank", "noopener,noreferrer")}><ArrowUpRight class="size-4" />Open in new tab</DropdownMenuItem></Show>
        <DropdownMenuItem class="min-h-11" onSelect={() => copied(href())}><Copy class="size-4" />Copy link</DropdownMenuItem>
        <Show when={target().kind === "thread"}><DropdownMenuItem class="min-h-11" onSelect={() => { const ref = target(); if (ref.kind === "thread") copied(threadReference(ref)); }}><Copy class="size-4" />Copy reference</DropdownMenuItem></Show>
        <DropdownMenuItem class="min-h-11" onSelect={() => void remove()}><X class="size-4" />Remove from Related</DropdownMenuItem>
      </DropdownMenuContent></DropdownMenu>
    </div><Show when={error()}><p role="alert" class="mt-1 text-sm text-destructive">{error()}</p></Show>
  </li>;
}
export function RelatedList(props: { origin: RelatedOrigin }) {
  const list = () => relatedState.lists[props.origin.threadId];
  const [adding, setAdding] = createSignal<"url" | "thread" | null>(null);
  const [url, setUrl] = createSignal("");
  const [title, setTitle] = createSignal("");
  const [query, setQuery] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const matches = () => filterThreadCandidates(threadState.ready ? threadState.threads : [], query().replace(/^#/, ""), 12);
  onSettled(() => { if (!list()?.loaded && !list()?.loading) void loadRelated(props.origin); });
  async function add(target: RelatedTarget, label: string | null) {
    if (busy()) return;
    setError(null);
    if (hasRelatedTarget(props.origin, target)) { setError("This reference is already in Related."); return; }
    setBusy(true);
    try { await addRelatedItem(props.origin, target, label); setUrl(""); setTitle(""); setQuery(""); setAdding(null); toast(`Saved to Related in Thread #${props.origin.threadId}`); }
    catch (failure) { setError(failure instanceof Error ? failure.message : String(failure)); }
    finally { setBusy(false); }
  }
  function submitUrl(event: SubmitEvent) {
    event.preventDefault(); const parsed = parseThreadLink(url());
    if (parsed && parsed.kind !== "thread") { setError("Use a complete Thread link copied from its actions."); return; }
    void add(parsed?.kind === "thread" ? parsed.target : {kind:"url",url:url()}, parsed?.kind === "thread" ? null : title().trim() || null);
  }
  return <section data-slot="related-list" class="mx-auto w-full max-w-measure">
    <header class="mb-5 flex items-center justify-between gap-3"><h2 class="text-lg font-medium">Related</h2><DropdownMenu><DropdownMenuTrigger class={control} aria-label="Add to Related"><Plus class="size-4" />Add</DropdownMenuTrigger><DropdownMenuContent><DropdownMenuItem class="min-h-11" onSelect={() => { setError(null); setAdding("url"); }}>Add link</DropdownMenuItem><DropdownMenuItem class="min-h-11" onSelect={() => { setError(null); setAdding("thread"); }}>Add thread</DropdownMenuItem></DropdownMenuContent></DropdownMenu></header>
    <Show when={adding() === "url"}><form class="mb-6 space-y-3" onSubmit={submitUrl}>
      <label class="block space-y-1 text-sm"><span>URL</span><input class={input} required maxlength={4096} inputmode="url" autocomplete="url" placeholder="https://…" value={url()} onInput={event => setUrl(event.currentTarget.value)} /></label>
      <label class="block space-y-1 text-sm"><span>Title <span class="text-muted-foreground">(optional for web links)</span></span><input class={input} maxlength={200} value={title()} onInput={event => setTitle(event.currentTarget.value)} /></label>
      <div class="flex gap-2"><button class={control} disabled={busy()} type="submit">{busy() ? "Saving…" : "Save link"}</button><button class={control} type="button" onClick={() => setAdding(null)}>Cancel</button></div>
    </form></Show>
    <Show when={adding() === "thread"}><div class="mb-6 space-y-3"><label class="block space-y-1 text-sm"><span>Find a thread</span><input class={input} type="search" placeholder="Title or #number" value={query()} onInput={event => setQuery(event.currentTarget.value)} /></label><ul class="max-h-64 overflow-y-auto"><For each={matches()}>{thread => <li><button class={`${control} w-full justify-between text-left`} disabled={busy() || hasRelatedTarget(props.origin,{kind:"thread",history_id:props.origin.historyId,thread_id:thread.id})} onClick={() => void add({kind:"thread",history_id:props.origin.historyId,thread_id:thread.id},null)}><span class="min-w-0 truncate">{thread.title}</span><span class="shrink-0 text-xs">#{thread.id}</span></button></li>}</For></ul><Show when={matches().length === 0}><p class="text-sm text-muted-foreground">No matching threads.</p></Show><button class={control} onClick={() => setAdding(null)}>Cancel</button></div></Show>
    <Show when={error()}><p role="alert" class="mb-4 text-sm text-destructive">{error()}</p></Show>
    <section aria-labelledby={`saved-references-${props.origin.threadId}`}><h3 id={`saved-references-${props.origin.threadId}`} class="text-sm font-medium">Saved references</h3>
      <Show when={list()?.loading}><p role="status" class="mt-3 text-sm text-muted-foreground">Loading references…</p></Show>
      <Show when={list()?.error}><div role="alert" class="mt-3 text-sm"><p>{list()?.error}</p><button class={control} onClick={() => void loadRelated(props.origin)}>Retry loading references</button></div></Show>
      <Show when={list()?.loaded && !list()?.error && list()?.items.length === 0}><p class="mt-3 text-sm leading-relaxed text-muted-foreground">Keep useful links and threads here. Add one above or use Link actions in a message.</p></Show>
      <ul class="mt-2 divide-y divide-border"><For each={list()?.items ?? []}>{item => <SavedReference item={item} origin={props.origin} />}</For></ul>
    </section>
    <section class="mt-8"><ArtifactList threadId={props.origin.threadId} embedded /></section>
  </section>;
}
