import { createEffect, createSignal, For, onCleanup, Show } from "solid-js";
import { Plus, X } from "../components/ui/icons";
import { createOverlayPresence } from "../lib/focus";
import { historyId } from "../lib/history";
import { filterThreadCandidates } from "../lib/thread-ref";
import { threadState } from "../threads/store";
import { ThreadAvatar } from "../threads/ThreadAvatar";
import { closeThreadReach, reachDialogTitle, threadReachTarget } from "./reach";
import { grantLabel, grantReach, holdsRoot, revokeReach, threadGrants, type GrantOrigin } from "./store";
import type { ReachTarget } from "../threads/types";

const remove = "inline-flex size-8 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-40 pointer-coarse:size-11";
const option = "flex min-h-11 w-full items-center justify-between gap-2 rounded-md px-2 py-1.5 text-left text-sm transition-colors hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-40";
const row = "flex min-h-9 items-start gap-2 rounded-md px-2 py-1.5 text-sm";
const field = "h-9 w-full min-w-0 rounded-md border border-border bg-background px-2 text-sm font-normal normal-case tracking-normal text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:h-11";

interface Candidate { key: string; label: string; detail: string; target: ReachTarget }

/**
 * Reach, read and edited in one place. Default reach is this Thread and
 * everything below it; each grant names one more Thread whose subtree is
 * addressable too, and a root grant is everything in the history, including
 * Threads made after the grant.
 */
export function ReachDialog() {
  let dialog: HTMLDialogElement | undefined;
  let search: HTMLInputElement | undefined;
  let restore: HTMLElement | null = null;
  const [query, setQuery] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const origin = (): GrantOrigin | null => {
    const target = threadReachTarget();
    return target ? { historyId: target.history, threadId: target.thread.id } : null;
  };
  const grants = () => { const id = origin()?.threadId; return id === undefined ? [] : threadGrants(id); };
  const root = () => { const id = origin()?.threadId; return id !== undefined && holdsRoot(id); };
  const held = () => new Set(grants().flatMap(grant => grant.target.kind === "thread" ? [grant.target.thread_id] : []));
  // Root is the first offer, because it is the one choice that also covers
  // Threads that do not exist yet; the Thread picker follows it.
  const candidates = (): Candidate[] => {
    if (root()) return [];
    const self = origin()?.threadId;
    const text = query().trim().replace(/^#/, "");
    const rootOffer: Candidate[] = text === "" || "everything root".includes(text.toLowerCase())
      ? [{ key: "root", label: "Everything (root)", detail: "every Thread, including later ones", target: "root" }]
      : [];
    return [...rootOffer, ...filterThreadCandidates(threadState.ready ? threadState.threads : [], text, 12)
      .filter(thread => thread.id !== self && !held().has(thread.id))
      .map(thread => ({ key: `thread-${thread.id}`, label: thread.title, detail: `#${thread.id}`, target: thread.id as ReachTarget }))];
  };
  const close = () => closeThreadReach();
  async function run(action: () => Promise<void>) {
    if (busy()) return;
    setBusy(true); setError(null);
    try { await action(); setQuery(""); }
    catch (failure) { setError(failure instanceof Error ? failure.message : String(failure)); }
    finally { setBusy(false); }
  }
  const add = (target: ReachTarget) => { const at = origin(); if (at) void run(() => grantReach(at, target, null)); };
  const drop = (target: ReachTarget) => { const at = origin(); if (at) void run(() => revokeReach(at, target)); };

  createOverlayPresence(() => threadReachTarget() !== null);
  onCleanup(() => { dialog?.close(); close(); });
  createEffect(() => ({ target: threadReachTarget(), history: historyId() }), ({ target, history }) => {
    if (target && target.history !== history) { close(); return; }
    if (target) {
      setQuery(""); setError(null); setBusy(false);
      restore = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      if (!dialog?.open) dialog?.showModal();
      const frame = requestAnimationFrame(() => search?.focus());
      return () => cancelAnimationFrame(frame);
    } else if (dialog?.open) {
      dialog.close();
      if (restore?.isConnected) restore.focus();
    }
  });

  return <dialog ref={node => { dialog = node; }} data-slot="thread-reach" aria-label={threadReachTarget() ? reachDialogTitle(threadReachTarget()!.thread) : "Thread reach"}
    onCancel={event => { event.preventDefault(); close(); }}
    onKeyDown={event => { if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); close(); } }}
    onPointerDown={event => { if (event.target === dialog) close(); }}
    class="m-auto max-h-[calc(100dvh-2rem)] w-[min(30rem,calc(100vw-2rem))] max-w-none flex-col overflow-y-auto rounded-xl border border-border bg-surface p-0 text-foreground shadow-raised backdrop:bg-scrim open:flex">
    <Show when={threadReachTarget()}>{target => <div class="flex min-h-0 flex-col gap-3 p-4">
      <h2 class="text-lg font-medium text-foreground">{reachDialogTitle(target().thread)}</h2>
      <ul class="flex flex-col gap-0.5 text-sm">
        <li class={`${row} text-muted-foreground`}>Itself and everything below it</li>
        <Show when={root()}>
          <li class={row}>
            <span class="min-w-0 flex-1">Everything (root)</span>
            <button type="button" class={remove} disabled={busy()} aria-label="Remove reach to everything" title="Remove this reach" onClick={() => drop("root")}><X class="size-3.5" /></button>
          </li>
        </Show>
        <For each={grants()}>{grant => <Show when={grant.target.kind === "thread" ? grant.target : null}>{thread => <li class={row}>
          <Show when={threadState.threads.find(candidate => candidate.id === thread().thread_id)}>{found => <ThreadAvatar thread={found()} dense />}</Show>
          <span class="min-w-0 flex-1 wrap-break-word" title={grant.note ?? undefined}>{grantLabel(thread())}</span>
          <button type="button" class={remove} disabled={busy()} aria-label={`Remove reach to Thread ${thread().thread_id}`} title="Remove this reach" onClick={() => drop(thread().thread_id)}><X class="size-3.5" /></button>
        </li>}</Show>}</For>
      </ul>
      <Show when={!root()} fallback={<p class="text-meta text-muted-foreground">Root reach already covers every Thread. Remove it to grant single Threads again.</p>}>
        <label class="flex flex-col gap-1 text-meta text-muted-foreground">
          <span class="inline-flex items-center gap-1"><Plus class="size-3" />Add reach</span>
          <input ref={node => { search = node; }} class={field} type="search" aria-label="Add reach to another Thread" placeholder="Everything, a title, or #number" value={query()}
            onInput={event => setQuery(event.currentTarget.value)}
            onKeyDown={event => {
              if (event.key !== "Enter" || event.isComposing) return;
              event.preventDefault();
              const first = candidates()[0];
              if (first) add(first.target);
            }} />
        </label>
        <ul class="max-h-56 overflow-y-auto">
          <For each={candidates()}>{candidate => <li>
            <button type="button" class={option} disabled={busy()} onClick={() => add(candidate.target)}>
              <Show when={typeof candidate.target === "number" ? threadState.threads.find(thread => thread.id === candidate.target) : null}>{found => <ThreadAvatar thread={found()} dense />}</Show>
              <span class="min-w-0 flex-1 wrap-break-word">{candidate.label}</span>
              <span class="shrink-0 text-meta tabular-nums text-muted-foreground">{candidate.detail}</span>
            </button>
          </li>}</For>
        </ul>
        <Show when={candidates().length === 0}><p class="text-sm text-muted-foreground">No other Threads to reach.</p></Show>
      </Show>
      <Show when={error()}><p role="alert" class="text-sm text-destructive">{error()}</p></Show>
    </div>}</Show>
  </dialog>;
}
