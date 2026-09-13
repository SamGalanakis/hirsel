import { createSignal, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { ArrowUpRight, Check, Copy, MessageCircle, Plus } from "../components/ui/icons";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, useDropdownTrigger } from "../components/ui/dropdown-menu";
import { historyId } from "../lib/history";
import { parseThreadLink, plainPrimaryClick, threadReference, threadUrl } from "../lib/thread-url";
import { focusThread, threadState } from "../threads/store";
import { ThreadAvatar, type ThreadAvatarIdentity } from "../threads/ThreadAvatar";
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

/* `size-[1.05em]` on the link icons is em-relative on purpose: an inline
 * reference's icon is sized by the sentence it sits in, not by the ramp. */
/** A Thread citation is ONE object: the Thread's own avatar and the Thread's own
 * name, on a quiet ground, at the size of the sentence citing it. The four-part
 * form this replaced — a loose avatar, an underlined `#1`, a chevron, and the
 * title the agent had written out beside them — read as four things in a row
 * and said the name twice. The id is no longer drawn: it lives in the accessible
 * name and the tooltip, which is also where `· unavailable` goes rather than
 * mid-sentence. Clicking the chip opens its actions, "Open" first; a modified
 * click is left to the browser, so ⌘-click still opens the Thread in a tab.
 *
 * The chip has no ground, padding, leading or baseline shift of its own: it is
 * the tile and the name, exactly as tall as the line it sits in, with no pill
 * behind it to open a gap before the punctuation that follows; the label —
 * `self-baseline`, the one item that sets the flex container's baseline —
 * keeps the sentence's own baseline, so the run trace's `text-xs` and prose's
 * `text-sm` both hold their rhythm. The pointer gets the same 1px hairline a
 * web link gets, under the name alone. */
function ThreadChip(props: { href: string; name: string; text: string; thread?: ThreadAvatarIdentity; actionable: boolean }) {
  const trigger = useDropdownTrigger();
  return <a href={props.href} ref={trigger.ref} target="_blank" rel="noopener noreferrer nofollow"
    aria-label={props.name} title={props.name}
    aria-haspopup={props.actionable ? "menu" : undefined} aria-expanded={props.actionable ? (trigger.open() ? "true" : "false") : undefined}
    class="group/chip inline-flex max-w-full items-center gap-[0.25em] rounded-sm no-underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
    onClick={event => { if (!props.actionable || !plainPrimaryClick(event)) return; event.preventDefault(); trigger.toggle(); }}
    onKeyDown={props.actionable ? trigger.onKeyDown : undefined}>
    <Show when={props.thread} fallback={<LinkIcon kind="thread" class="size-[1.05em] shrink-0" />}>{thread => <ThreadAvatar thread={thread()} inline />}</Show>
    <span data-slot="thread-chip-label" class="self-baseline truncate decoration-1 decoration-current/25 underline-offset-2 group-hover/chip:underline">{props.text}</span>
  </a>;
}

/** A web link stays prose: t3code draws links with no decoration at rest and a
 * faint underline only under the pointer, and a 1px hairline at 25% keeps the
 * affordance without relying on colour alone. Its actions occupy no inline
 * space at all — right-click, long-press or Shift+F10 opens them, the same
 * gesture the Thread inventory rows already answer to. */
function PlainLink(props: { href: string; title: string; actionable: boolean; children: JSX.Element }) {
  const trigger = useDropdownTrigger();
  return <a href={props.href} ref={trigger.ref} target="_blank" rel="noopener noreferrer nofollow" title={props.title}
    aria-haspopup={props.actionable ? "menu" : undefined} aria-expanded={props.actionable ? (trigger.open() ? "true" : "false") : undefined}
    class="rounded-sm underline decoration-1 decoration-current/25 underline-offset-2 transition-[text-decoration-color] hover:decoration-current/80 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
    onContextMenu={event => { if (!props.actionable) return; event.preventDefault(); trigger.toggle(); }}>
    {props.children}
  </a>;
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
  const label = () => threadTarget() ? `Thread #${threadTarget()!.thread_id} · ${localThread()?.title ?? "unavailable"}`
    : !bare() ? props.label : link()?.label ?? props.label;
  async function save() {
    if (!origin || !canSave() || busy() || saved()) return;
    setBusy(true); setError(null);
    const resource = target()!;
    try { await addRelatedItem(origin, resource, resource.kind === "thread" || bare() ? null : [...props.label].slice(0, 200).join("")); toast(`Saved to Related in Thread #${origin.threadId}`); }
    catch (failure) { setError(failure instanceof Error ? failure.message : String(failure)); }
    finally { setBusy(false); }
  }
  const copied = (value: string) => { setError(null); void copyLink(value).catch(failure => setError(failure.message)); };
  return <DropdownMenu><span data-link-kind={threadTarget() || parsed() ? "thread" : link()?.kind} class="inline">
    <Show when={threadTarget()} fallback={
      <PlainLink href={href()} title={props.title ?? href()} actionable={target() !== null}>
        <Show when={!props.imageOnly && link()}><span class="mr-[0.25em] inline-flex align-text-bottom"><LinkIcon kind={link()!.kind} class="size-[1.05em] shrink-0" /></span></Show>
        <Show when={bare() && target()} fallback={props.children}>{label()}</Show>
      </PlainLink>
    }>{ref =>
      <ThreadChip href={href()} name={label()} text={localThread()?.title ?? `#${ref().thread_id}`} thread={localThread()} actionable={target() !== null} />
    }</Show>
    <Show when={target()}><DropdownMenuContent>
      <Show when={localThread()}><DropdownMenuItem class="min-h-11" onSelect={() => focusThread(localThread()!.id)}><MessageCircle class="size-4" />Open</DropdownMenuItem></Show>
      <DropdownMenuItem class="min-h-11" onSelect={() => window.open(href(), "_blank", "noopener,noreferrer")}><ArrowUpRight class="size-4" />Open in new tab</DropdownMenuItem>
      <DropdownMenuItem class="min-h-11" onSelect={() => copied(href())}><Copy class="size-4" />Copy link</DropdownMenuItem>
      <Show when={threadTarget()}><DropdownMenuItem class="min-h-11" onSelect={() => copied(threadReference(threadTarget()!))}><Copy class="size-4" />Copy reference</DropdownMenuItem></Show>
      <Show when={origin}><DropdownMenuItem class="min-h-11" disabled={busy() || saved() || !canSave()} onSelect={() => void save()}><Show when={saved()} fallback={<Plus class="size-4" />}><Check class="size-4" /></Show>{saved() ? "Already in Related" : busy() ? "Saving…" : !canSave() ? "Thread unavailable" : "Add to Related"}</DropdownMenuItem></Show>
    </DropdownMenuContent></Show>
    <Show when={error()}><span role="alert" class="ml-2 text-meta text-destructive">{error()}</span></Show>
  </span></DropdownMenu>;
}
