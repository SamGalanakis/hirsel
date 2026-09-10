import { ShowcaseButton, ShowcaseSurface } from "../artifacts/ShowcaseSurface";
import { RelatedContext } from "../related/context";
import { RelatedList } from "../related/RelatedList";
import { relatedState } from "../related/store";
import { anyOverlayOpen } from "../lib/focus";
import { consumeDraftArtifact, draftArtifact, stageDraftArtifact } from "../artifacts/draft-context";
import { historyId, recoveredDrafts } from "../lib/history";
import { ArtifactCard, ArtifactList, ArtifactSurface } from "../artifacts/ArtifactSurface";
import { createEffect, createMemo, createRoot, createSignal, For, onCleanup, onSettled, Show } from "solid-js";

import { Markdown } from "../components/Markdown";
import { Composer } from "../components/chat/Composer";
import { createComposerAttachments, type AttachmentsController } from "../components/chat/useAttachments";
import { ThreadMessage } from "./ThreadMessages";
import { conversationEntries } from "./conversation";
import { emptyHistory } from "./model";
import { BrandMark } from "../components/BrandMark";
import { Activity, Settings, GitBranch, LayoutGrid, ArrowLeft, MessageCircle, FileText, Plus } from "../components/ui/icons";
import { ThreadLink } from "./ThreadRef";
import { ThreadStatus } from "./ThreadStatus";
import { threadAncestors } from "./tree";
import { ThreadError } from "./ThreadError";
import { ThreadAvatar } from "./ThreadAvatar";
import { ThreadIconPicker } from "./ThreadIconPicker";
import { ThreadActions } from "./ThreadActions";
import { SettingsSheet } from "../components/settings/SettingsSheet";
import { ProcessesSheet } from "../components/processes/ProcessesSheet";
import { CanvasRail, CanvasSheet, CanvasButton } from "../components/views/CanvasSurface";
import { ConnectionPill } from "../components/ConnectionPill";
import { openProcesses, openSettings, state } from "../store/store";
import { ThreadInstrument } from "../views/ThreadInstrument";
import { getClient } from "../ws/client";
import { ThreadNavigation } from "./ThreadNavigation";
import { threadNavigationOpen as navigationOpen, threadNavigationIntent, openThreadNavigation, closeThreadNavigation } from "./navigation";
import { artifactState } from "../artifacts/store";
import { createThread, focusThread, followThreadLocation, openThread, retryThreadMessage, sendThreadMessage, threadAction, threadState } from "./store";

const button = "inline-flex min-h-11 items-center justify-center gap-2 rounded-lg px-3 py-2 text-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-50";
const iconButton = "inline-flex size-11 shrink-0 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring aria-pressed:bg-muted aria-pressed:text-foreground";
function ThreadConversation(props: { id: number; historyId: string; attachments: AttachmentsController; globalArtifacts: boolean; onConversation: () => void; onBrowse: () => void }) {
  const [now, setNow] = createSignal(Date.now());
  const statusTimer = setInterval(() => setNow(Date.now()), 30_000);
  onCleanup(() => clearInterval(statusTimer));
  const [relatedView, setRelatedView] = createSignal(false);
  const showRelated = () => relatedView() || props.globalArtifacts;
  const attachments = props.attachments;
  const current = () => threadState.threads.find(t => t.id === props.id);
  const history = () => threadState.histories[props.id];
  const messages = () => history()?.messages ?? [];
  const entries = createMemo(() => conversationEntries(history() ?? emptyHistory()));
  const pending = () => threadState.pending.filter(p => p.threadId === props.id);
  const thinking = () => history()?.turns.some(t => t.state === "running" || t.state === "queued") ?? false;
  const [loading, setLoading] = createSignal(false);
  let scroller: HTMLDivElement | undefined;
  let following = true;
  createEffect(() => messages().length + pending().length + (history()?.turns.reduce((count, turn) => count + (threadState.turnDetails[turn.id]?.length ?? 0), 0) ?? 0), (count) => {
    if (following && count >= 0) requestAnimationFrame(() => { if (scroller) scroller.scrollTop = scroller.scrollHeight; });
  });
  onSettled(() => {
    const observer = new ResizeObserver(() => {
      if (following && scroller) scroller.scrollTop = scroller.scrollHeight;
    });
    if (scroller) observer.observe(scroller);
    return () => observer.disconnect();
  });
  const earlier = async () => {
    const first = messages()[0];
    if (!first || loading()) return;
    setLoading(true);
    const height = scroller?.scrollHeight ?? 0;
    try {
      await openThread(props.id, first.id);
      requestAnimationFrame(() => { if (scroller) scroller.scrollTop += scroller.scrollHeight - height; });
    } catch { /* openThread owns contextual recovery. */ }
    finally { setLoading(false); }
  };
  const origin = { historyId: props.historyId, threadId: props.id };
  const artifactCount = () => artifactState.summaries.filter(artifact => artifact.thread_ids.includes(props.id)).length;
  const relatedCount = () => artifactCount() + (relatedState.lists[props.id]?.items.length ?? 0);
  return <RelatedContext value={origin}><main class="thread-focus-frame flex min-h-0 min-w-0 flex-1 flex-col rounded-xl border border-border bg-background" data-thread-id={props.id} aria-label={current()?.title ?? "Thread conversation"}>
      <header class="flex min-h-14 shrink-0 items-center gap-0.5 border-b border-border/60 px-1.5 sm:gap-1 sm:px-2" data-slot="thread-context">
        <button class={iconButton} aria-label={props.globalArtifacts ? "Back to conversation" : "Browse threads"} title={props.globalArtifacts ? "Back to conversation" : "Browse threads"} onClick={() => { if (props.globalArtifacts) props.onConversation(); else props.onBrowse(); }}><ArrowLeft class="size-4" /></button>
        <ThreadAvatar thread={{ id: props.id, title: current()?.title ?? "Thread", icon: current()?.icon }} />
        <h1 class="min-w-0 flex-1 text-sm font-medium"><button class="block min-h-11 w-full truncate rounded-lg px-1 text-left hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" aria-label={current()?.title ?? "Loading thread…"} title="Show full thread name in Threads" aria-haspopup="dialog" aria-controls="thread-navigation" onClick={() => openThreadNavigation()}><span class="flex min-w-0 items-center gap-1.5"><span class="shrink-0 whitespace-nowrap text-xs tabular-nums text-muted-foreground">#{props.id}</span><span class="truncate">{current()?.title ?? "Loading thread…"}</span></span></button></h1>
        <Show when={current()?.attention === "needs_owner"}><span class="size-2 shrink-0 rounded-full bg-status-attention" role="status" aria-label="Needs you" title="Needs you" /></Show>
        <Show when={current()?.settled_at}><span class="hidden text-xs text-muted-foreground sm:inline">Settled</span></Show>
        <button class={iconButton} aria-label="Conversation" title="Conversation" aria-pressed={!showRelated() ? "true" : "false"} onClick={() => { setRelatedView(false); props.onConversation(); }}><MessageCircle class="size-4" /></button>
        <button class={`${iconButton} relative`} aria-label="Related" title="Related links and artifacts" aria-pressed={relatedView() && !props.globalArtifacts ? "true" : "false"} onClick={() => { props.onConversation(); setRelatedView(true); }}><FileText class="size-4" /><Show when={relatedCount() > 0}><span aria-hidden="true" class="absolute top-0.5 right-0.5 grid min-w-3.5 place-items-center rounded-full bg-muted px-0.5 text-[10px] tabular-nums">{relatedCount()}</span></Show></button>
        <ShowcaseButton threadId={props.id} />
        <CanvasButton />
        <Show when={current()}>
          <ThreadActions thread={current()!} />
        </Show>
      </header>
    <div ref={node => { scroller = node; }} class="min-h-0 flex-1 overflow-y-auto px-3 py-6 sm:px-gutter" data-slot="thread-scroll" onScroll={() => { if (scroller) following = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 80; }}>
      <Show when={!showRelated()} fallback={<Show when={props.globalArtifacts} fallback={<RelatedList origin={origin} />}><ArtifactList onResume={props.onConversation} /></Show>}>
      <div class="mx-auto flex w-full max-w-measure flex-col gap-6">
        <Show when={current()?.parent_thread_id !== null && current()?.parent_thread_id !== undefined}><nav aria-label="Thread ancestry" class="flex flex-wrap items-center gap-1 text-xs text-muted-foreground"><For each={threadAncestors(threadState.threads, props.id)}>{parent => <><ThreadLink id={parent.id} /><span aria-hidden="true">/</span></>}</For><span class="break-words">#{props.id} {current()?.title}</span></nav></Show>
        <Show when={threadState.threads.some(thread => thread.parent_thread_id === props.id)}><details data-slot="child-threads" class="text-sm"><summary class="min-h-11 w-fit cursor-pointer rounded py-3 text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">Child threads · {threadState.threads.filter(thread => thread.parent_thread_id === props.id).length}</summary><ul class="space-y-1"><For each={threadState.threads.filter(thread => thread.parent_thread_id === props.id)}>{child => <li class="flex flex-wrap items-center gap-2"><ThreadLink id={child.id} /><ThreadStatus thread={child} now={now()} /></li>}</For></ul></details></Show>
        <Show when={history()?.brief.text || history()?.brief.artifact_ids.length}><details data-slot="thread-brief" class="text-sm"><summary class="min-h-11 w-fit cursor-pointer rounded py-3 text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">Current brief</summary><div class="space-y-2 pb-2"><Markdown>{history()?.brief.text ?? ""}</Markdown><For each={history()?.brief.artifact_ids ?? []}>{id => <ArtifactCard id={id} />}</For></div></details></Show>
        <Show when={current()?.description}><p class="text-sm text-muted-foreground">{current()?.description}</p></Show>
        <Show when={current()?.instrument && Object.keys(current()!.instrument!).length > 0}>
          <Show when={current()?.revision} keyed>{revision => <ThreadInstrument ui={current()?.instrument ?? undefined} onAction={(action, data) => threadAction(props.historyId, props.id, action, data, revision)} />}</Show>
        </Show>
        <Show when={history()?.hasMore}><button class={button} disabled={loading()} onClick={() => void earlier()}>{loading() ? "Loading…" : "Load earlier messages"}</button></Show>
        <Show when={!history()?.loaded && !(threadState.error?.operation === "load" && threadState.error.threadId === props.id)}><p role="status" class="text-sm text-muted-foreground">Loading conversation…</p></Show>
        <Show when={history()?.loaded && messages().length === 0 && pending().length === 0 && !thinking()}><p class="text-sm text-muted-foreground">Start the conversation for this thread.</p></Show>
        <For each={entries()} keyed={entry => entry.key}>{entry => <ThreadMessage entry={entry()} history={history() ?? emptyHistory()} threadId={props.id} />}</For>
        <For each={pending()} keyed={message => message.clientId}>{message => <PendingMessageRow message={message()} />}</For>

      </div>
      </Show>
    </div>
    <ThreadError threadId={props.id} />
    <Composer artifactContext={draftArtifact(props.id)} onRemoveArtifactContext={() => stageDraftArtifact(props.id, null)} onConsumeArtifactContext={id => consumeDraftArtifact(props.id, id)} ariaLabel={`Message ${current()?.title ?? "this Thread"}`} draftKey={`${historyId()}:thread-${props.id}`} attachments={attachments} thinking={thinking()} focused threads={threadState.threads}
      onSend={(body, mode, blobs, mentions, artifactIds) => {
        sendThreadMessage(props.historyId, props.id, body, mode, blobs, mentions, artifactIds);
      }}
      onStop={() => getClient()?.cancelTurn(props.historyId, props.id)} getLastOwnerBody={() => messages().findLast(m => m.author === "owner")?.body ?? null} />
  </main></RelatedContext>;
}

function PendingMessageRow(props: { message: (typeof threadState.pending)[number] }) {
  const originHistory = historyId();
  const { clientId, threadId } = props.message;
  let retryButton: HTMLButtonElement | undefined;
  let ownsFocus = false;
  const movedFocus = (event: FocusEvent) => { if (event.target !== retryButton) ownsFocus = false; };
  document.addEventListener("focusin", movedFocus);
  onCleanup(() => {
    document.removeEventListener("focusin", movedFocus);
    if (!ownsFocus) return;
    queueMicrotask(() => {
      const accepted = threadState.histories[threadId]?.messages.some(message => message.author === "owner" && message.client_id === clientId);
      if (!accepted || historyId() !== originHistory || threadState.focusedId !== threadId || anyOverlayOpen()) return;
      if (document.activeElement !== document.body && document.activeElement !== retryButton) return;
      document.querySelector<HTMLTextAreaElement>(`main[data-thread-id="${threadId}"] [data-composer="main"]`)?.focus();
    });
  });
  return <article class={`ml-6 rounded-xl bg-muted/65 px-4 py-3 ${props.message.failed ? "" : "opacity-70"}`}><Markdown>{props.message.body}</Markdown><For each={props.message.artifactIds}>{id => <ArtifactCard id={id} />}</For><span class="text-xs">{props.message.failed ? "Failed to send" : state.connection === "connected" ? "Sending…" : "Waiting for connection…"}</span><Show when={props.message.failed}><button class={button} ref={node => { retryButton = node; }} onFocus={() => { ownsFocus = true; }} onClick={() => retryThreadMessage(props.message.clientId)}>Retry</button></Show></article>;
}

function ThreadStart(props: { globalArtifacts: boolean; onSelect: (id: number) => void }) {
  const [title, setTitle] = createSignal("");
  const [creating, setCreating] = createSignal(false);
  const [error, setError] = createSignal("");
  const create = async (event: SubmitEvent) => {
    event.preventDefault(); if (!title().trim() || creating()) return;
    setCreating(true); setError("");
    const expectedHistory = historyId();
    if (!expectedHistory) { setError("History is unavailable. Reconnect and try again."); setCreating(false); return; }
    try { const thread = await createThread(expectedHistory, title().trim(), null); setTitle(""); props.onSelect(thread.id); }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)); }
    finally { setCreating(false); }
  };
  return <main data-slot="thread-empty" class="flex min-h-0 min-w-0 flex-1 flex-col overflow-y-auto p-5 sm:p-8">
    <Show when={props.globalArtifacts} fallback={<div class="m-auto w-full max-w-md space-y-4">
      <Show when={threadState.linkError} fallback={<>
      <h1 class="text-lg font-medium">{threadState.focusedId !== null ? `Thread #${threadState.focusedId} is unavailable` : threadState.threads.length ? "Choose a thread" : "Start a thread"}</h1>
      <p class="text-sm text-muted-foreground">{threadState.threads.length ? "Open a conversation from Threads, or start a new one." : "Give your conversation a name. You can organize focused work inside it later."}</p>
      <Show when={threadState.threads.length > 0}><button class={button} onClick={() => openThreadNavigation()}>Browse threads</button></Show>
      <form class="flex gap-2" onSubmit={event => void create(event)}><input aria-label="First thread title" placeholder="Thread name" value={title()} onInput={event => setTitle(event.currentTarget.value)} class="min-w-0 flex-1 rounded-lg border border-border bg-transparent px-3 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" /><button type="submit" class={button} disabled={!title().trim() || creating() || state.connection !== "connected"}>Create thread</button></form>
      <Show when={error()}><p role="alert" class="text-sm text-destructive">{error()}</p></Show>
      </>}><h1 class="text-lg font-medium">{threadState.linkError}</h1><p class="text-sm text-muted-foreground">Your drafts are kept. Choose a conversation from this history to continue.</p><button class={button} onClick={() => { focusThread(null); openThreadNavigation(); }}>Return to threads</button></Show>
    </div>}><ArtifactList /></Show>
  </main>;
}

export function ThreadShell() {
  const [globalArtifacts, setGlobalArtifacts] = createSignal(false);
  const [attentionNow, setAttentionNow] = createSignal(Date.now());
  const needsAttention = (thread: (typeof threadState.threads)[number]) => !thread.archived_at && thread.attention === "needs_owner";
  const attentionCount = createMemo(() => threadState.threads.filter(thread => needsAttention(thread) && !(thread.snoozed_until && Date.parse(thread.snoozed_until) > attentionNow())).length);
  createEffect(() => ({ now: attentionNow(), deadlines: threadState.threads.filter(needsAttention).map(thread => Date.parse(thread.snoozed_until ?? "")) }), ({ now, deadlines }) => {
    const next = Math.min(...deadlines.filter(time => time > now));
    if (!Number.isFinite(next)) return;
    const timer = setTimeout(() => setAttentionNow(Date.now()), Math.min(next - now + 1, 2_147_483_647));
    return () => clearTimeout(timer);
  });
  onCleanup(() => closeThreadNavigation());
  const selectThread = (id: number) => { setGlobalArtifacts(false); closeThreadNavigation(); focusThread(id); };
  const composers = new Map<string, { attachments: AttachmentsController; dispose: () => void }>();
  const attachmentsFor = (id: number) => {
    const key = `${historyId()}:${id}`;
    if (!composers.has(key)) composers.set(key, createRoot(dispose => ({ attachments: createComposerAttachments(), dispose })));
    return composers.get(key)!.attachments;
  };
  createEffect(historyId, () => { for (const [key, composer] of composers) if (!key.startsWith(`${historyId()}:`)) { composer.dispose(); composers.delete(key); } });
  onCleanup(() => { for (const composer of composers.values()) composer.dispose(); });
  const onPop = () => followThreadLocation();
  window.addEventListener("popstate", onPop);
  onCleanup(() => window.removeEventListener("popstate", onPop));
  createEffect(() => { const thread = threadState.threads.find(t => t.id === threadState.focusedId); const history = historyId(); return thread && history && !thread.read && state.connection === "connected" ? { id: thread.id, history } : null; }, (target) => { if (target) threadAction(target.history, target.id, "read"); });
  return <div class="flex h-dvh min-h-0 bg-background text-foreground">
    <nav aria-label="Hirsel" data-slot="icon-rail" class="relative z-10 flex w-14 shrink-0 flex-col items-center gap-2 py-2">
      <button class={iconButton} aria-label="Thread overview" title="Thread overview" aria-pressed={threadState.focusedId === null && !globalArtifacts() ? "true" : "false"} onClick={() => { setGlobalArtifacts(false); focusThread(null); }}><BrandMark size={23} /></button>
      <button class={`${iconButton} relative`} aria-label="Threads" aria-describedby={attentionCount() > 0 ? "thread-attention-summary" : undefined} title={attentionCount() > 0 ? `Threads · ${attentionCount()} need you` : "Threads"} data-slot="thread-navigation-trigger" aria-controls="thread-navigation" aria-expanded={navigationOpen() ? "true" : "false"} aria-pressed={threadState.focusedId !== null && !globalArtifacts() ? "true" : "false"} onClick={() => navigationOpen() ? closeThreadNavigation() : openThreadNavigation()}><GitBranch class="size-5" /><Show when={attentionCount() > 0}><span aria-hidden="true" class="absolute right-2 top-2 size-1.5 rounded-full bg-status-attention" /><span id="thread-attention-summary" class="sr-only">{attentionCount()} {attentionCount() === 1 ? "thread needs" : "threads need"} your attention</span></Show></button>
      <Show when={threadState.focusedId !== null && !globalArtifacts()}><svg class="pointer-events-none absolute top-[58px] left-12 h-8 w-4 text-border" viewBox="0 0 16 32" fill="none" aria-hidden="true" data-slot="thread-connector"><path d="M0 24h4c8 0 12-4 12-12V0" stroke="currentColor" /></svg></Show>
      <button class={iconButton} aria-label="New thread" title="New thread" onClick={() => { const history = historyId(); if (history) openThreadNavigation({ kind: "create", historyId: history, parentId: null }); }}><Plus class="size-5" /></button>
      <button class={iconButton} aria-label="All artifacts" title="All artifacts" aria-pressed={globalArtifacts() ? "true" : "false"} onClick={() => setGlobalArtifacts(value => !value)}><LayoutGrid class="size-5" /></button>
      <button class={iconButton} aria-label="Processes" title="Processes" onClick={openProcesses}><Activity class="size-5" /></button>
      <div class="flex-1" />
      <button class={iconButton} aria-label="Settings" title="Settings" onClick={() => openSettings()}><Settings class="size-5" /></button>
    </nav>
    <ThreadIconPicker />
    <ThreadNavigation intent={threadNavigationIntent()} onClose={() => closeThreadNavigation()} onSelect={selectThread} />
    <div class="flex min-h-0 min-w-0 flex-1 flex-col">
      <Show when={recoveredDrafts().length > 0}><details class="px-3 py-2 text-sm"><summary class="cursor-pointer text-muted-foreground">Saved drafts from another history</summary><p class="py-2">Copy any text you want to keep into a new conversation.</p><For each={recoveredDrafts()}>{draft => <pre class="max-h-40 overflow-auto whitespace-pre-wrap rounded border border-border p-2 text-xs">{draft.text}</pre>}</For></details></Show>
      <Show when={state.connection !== "connected"}><div class="flex shrink-0 justify-end px-3 pt-2"><ConnectionPill /></div></Show>
      <div class="flex min-h-0 flex-1 gap-2 py-2 pr-2 pl-2 sm:gap-3 sm:pr-3">
        <Show when={threadState.ready && historyId() && threadState.focusedId !== null && threadState.threads.some(thread => thread.id === threadState.focusedId) ? { id: threadState.focusedId!, history: historyId()! } : null} keyed fallback={<ThreadStart globalArtifacts={globalArtifacts()} onSelect={selectThread} />} >{focused => <ThreadConversation id={focused.id} historyId={focused.history} attachments={attachmentsFor(focused.id)} globalArtifacts={globalArtifacts()} onConversation={() => setGlobalArtifacts(false)} onBrowse={() => openThreadNavigation()} />}</Show>
        <ArtifactSurface />
        <ShowcaseSurface />
        <CanvasRail /><CanvasSheet /><ProcessesSheet /><SettingsSheet />
      </div>
    </div>
  </div>;
}
