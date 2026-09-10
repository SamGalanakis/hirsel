import { createSignal, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { ArrowUpRight, Check, ChevronDown, Copy, MessageCircle, Plus } from "../components/ui/icons";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "../components/ui/dropdown-menu";
import { historyId } from "../lib/history";
import { parseThreadLink, plainPrimaryClick, threadReference, threadUrl } from "../lib/thread-url";
import { focusThread, threadState } from "../threads/store";
import { ThreadAvatar } from "../threads/ThreadAvatar";
import type { RelatedTarget } from "../threads/types";
import { toast } from "../lib/toast";
import { useRelatedOrigin } from "./context";
import { addRelatedItem, hasRelatedTarget } from "./store";
import { LinkIcon } from "./LinkIcon";
import { isBareLinkLabel, webLink } from "./url";
export async function copyLink(url: string): Promise<void> {
  try { await navigator.clipboard.writeText(url); toast("Link copied"); }
  catch { throw new Error("Couldn’t copy the link. Use your browser’s Copy link address action."); }
}
export function RichLink(props: { href: string; title?: string; label: string; children: JSX.Element; imageOnly?: boolean; target?: RelatedTarget }) {
  const origin = useRelatedOrigin();
  const parsed = () => parseThreadLink(props.href);
  const threadTarget = () => {
    if (props.target?.kind === "thread") return props.target;
    const result = parsed(); return result?.kind === "thread" ? result.target : null;
  };
  const link = () => parsed() ? null : webLink(props.href);
  const target = (): RelatedTarget | null => threadTarget() ?? (link() ? {kind:"url",url:link()!.url} : null);
  const localThread = () => { const ref = threadTarget(); return threadState.ready && ref?.history_id === historyId() ? threadState.threads.find(thread => thread.id === ref.thread_id) : undefined; };
  const bare = () => isBareLinkLabel(props.label, props.href);
  const href = () => threadTarget() ? threadUrl(threadTarget()!) : props.href;
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const saved = () => origin !== null && target() !== null && hasRelatedTarget(origin, target()!);
  const canSave = () => target() !== null && (!threadTarget() || localThread() !== undefined);
  const label = () => !bare() ? props.label : threadTarget() ? `Thread #${threadTarget()!.thread_id}${localThread() ? ` · ${localThread()!.title}` : " · unavailable"}` : link()?.label ?? props.label;
  async function save() {
    if (!origin || !canSave() || busy() || saved()) return;
    setBusy(true); setError(null);
    const resource = target()!;
    try { await addRelatedItem(origin, resource, resource.kind === "thread" || bare() ? null : [...props.label].slice(0, 200).join("")); toast(`Saved to Related in Thread #${origin.threadId}`); }
    catch (failure) { setError(failure instanceof Error ? failure.message : String(failure)); }
    finally { setBusy(false); }
  }
  const copied = (value: string) => { setError(null); void copyLink(value).catch(failure => setError(failure.message)); };
  return <span data-link-kind={threadTarget() || parsed() ? "thread" : link()?.kind} class="inline">
    <a href={href()} target="_blank" rel="noopener noreferrer nofollow" title={props.title ?? href()}
      class="rounded-sm underline decoration-dotted decoration-current/50 underline-offset-4 hover:decoration-solid hover:decoration-current focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      onClick={event => { if (localThread() && plainPrimaryClick(event)) { event.preventDefault(); focusThread(localThread()!.id); } }}>
      <Show when={!props.imageOnly && (threadTarget() || link())}><span class="mr-1 inline-flex align-text-bottom"><Show when={localThread()} fallback={<LinkIcon kind={threadTarget() ? "thread" : link()!.kind} />}>{thread => <ThreadAvatar thread={thread()} small />}</Show></span></Show>
      <Show when={bare() && target()} fallback={props.children}>{label()}</Show>
    </a>
    <Show when={target()}><DropdownMenu><DropdownMenuTrigger aria-label={`Link actions: ${label()}`} title="Link actions" class="ml-0.5 inline-flex size-6 items-center justify-center rounded align-middle text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring [@media(pointer:coarse)]:size-11"><ChevronDown class="size-3" /></DropdownMenuTrigger><DropdownMenuContent>
      <Show when={localThread()}><DropdownMenuItem class="min-h-11" onSelect={() => focusThread(localThread()!.id)}><MessageCircle class="size-4" />Open thread</DropdownMenuItem></Show>
      <DropdownMenuItem class="min-h-11" onSelect={() => window.open(href(), "_blank", "noopener,noreferrer")}><ArrowUpRight class="size-4" />Open in new tab</DropdownMenuItem>
      <DropdownMenuItem class="min-h-11" onSelect={() => copied(href())}><Copy class="size-4" />Copy link</DropdownMenuItem>
      <Show when={threadTarget()}><DropdownMenuItem class="min-h-11" onSelect={() => copied(threadReference(threadTarget()!))}><Copy class="size-4" />Copy reference</DropdownMenuItem></Show>
      <Show when={origin}><DropdownMenuItem class="min-h-11" disabled={busy() || saved() || !canSave()} onSelect={() => void save()}><Show when={saved()} fallback={<Plus class="size-4" />}><Check class="size-4" /></Show>{saved() ? "Already in Related" : busy() ? "Saving…" : !canSave() ? "Thread unavailable" : "Add to Related"}</DropdownMenuItem></Show>
    </DropdownMenuContent></DropdownMenu></Show>
    <Show when={error()}><span role="alert" class="ml-2 text-xs text-destructive">{error()}</span></Show>
  </span>;
}
