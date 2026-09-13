import { createSignal, For, Show } from "solid-js";
import { Plus, X } from "../components/ui/icons";
import { filterThreadCandidates } from "../lib/thread-ref";
import { threadState } from "../threads/store";
import { grantReach, revokeReach, threadGrants, type GrantOrigin } from "./store";

const control = "inline-flex min-h-8 items-center gap-1 rounded-lg px-1.5 text-meta text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50";
const input = "min-h-11 w-full min-w-0 rounded-lg border border-input bg-surface px-3 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring";

/**
 * What this Thread can address, on one line, before and during a turn. Reach is
 * durable state the Owner can read at a glance and change here: default reach
 * is self and everything below it, and each grant names one more Thread whose
 * subtree is addressable too.
 */
export function ReachStrip(props: { origin: GrantOrigin }) {
  const [adding, setAdding] = createSignal(false);
  const [query, setQuery] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const grants = () => threadGrants(props.origin.threadId);
  const held = () => new Set(grants().map(grant => grant.target_thread_id));
  const candidates = () => filterThreadCandidates(threadState.ready ? threadState.threads : [], query().replace(/^#/, ""), 12)
    .filter(thread => thread.id !== props.origin.threadId && !held().has(thread.id));
  async function run(action: () => Promise<void>) {
    if (busy()) return;
    setBusy(true); setError(null);
    try { await action(); setQuery(""); setAdding(false); }
    catch (failure) { setError(failure instanceof Error ? failure.message : String(failure)); }
    finally { setBusy(false); }
  }
  return <section data-slot="reach-strip" aria-label="Thread reach" class="shrink-0 px-4 pt-2 sm:px-gutter">
    <div class="mx-auto flex w-full max-w-measure flex-wrap items-center gap-x-1 gap-y-0.5 font-mono text-meta text-muted-foreground">
      <span class="shrink-0">Reach:</span>
      <span class="shrink-0">self + subtree</span>
      <For each={grants()}>{grant => <span class="inline-flex shrink-0 items-center">
        <span aria-hidden="true">·</span>
        <span class="ml-1" title={grant.note ?? undefined}>+Thread {grant.target_thread_id} '{grant.title}'</span>
        <button class={`${control} px-1`} disabled={busy()} aria-label={`Remove reach to Thread ${grant.target_thread_id}`} title="Remove this reach" onClick={() => void run(() => revokeReach(props.origin, grant.target_thread_id))}><X class="size-3" /></button>
      </span>}</For>
      <button class={`${control} ml-auto shrink-0`} aria-label="Add reach to another Thread" aria-expanded={adding() ? "true" : "false"} onClick={() => { setError(null); setAdding(open => !open); }}><Plus class="size-3" />Reach</button>
    </div>
    <Show when={adding()}>
      <div class="mx-auto mt-2 w-full max-w-measure space-y-2">
        <label class="block space-y-1 text-sm"><span>Grant reach to</span><input class={input} type="search" placeholder="Title or #number" value={query()} onInput={event => setQuery(event.currentTarget.value)} /></label>
        <ul class="max-h-48 overflow-y-auto">
          <For each={candidates()}>{thread => <li><button class="flex min-h-11 w-full items-center justify-between gap-2 rounded-lg px-2 text-left text-sm hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50" disabled={busy()} onClick={() => void run(() => grantReach(props.origin, thread.id, null))}><span class="min-w-0 truncate">{thread.title}</span><span class="shrink-0 font-mono text-meta text-muted-foreground">#{thread.id}</span></button></li>}</For>
        </ul>
        <Show when={candidates().length === 0}><p class="text-sm text-muted-foreground">No other Threads to reach.</p></Show>
      </div>
    </Show>
    <Show when={error()}><p role="alert" class="mx-auto w-full max-w-measure pt-1 text-sm text-destructive">{error()}</p></Show>
  </section>;
}
