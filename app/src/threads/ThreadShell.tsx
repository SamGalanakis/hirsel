import { ArtifactCard, ArtifactList, ArtifactSurface } from "../artifacts/ArtifactSurface";
import { splitStreamingReply } from "../components/chat/timeline";
import { createEffect, createMemo, createRoot, createSignal, For, onCleanup, onSettled, Show } from "solid-js";

import { Markdown } from "../components/Markdown";
import { Composer } from "../components/chat/Composer";
import { createComposerAttachments, type AttachmentsController } from "../components/chat/useAttachments";
import { ThreadExecution, ownerFacingActivity, activityText } from "./ThreadExecution";
import { BrandMark } from "../components/BrandMark";
import { Activity, Settings, GitBranch, LayoutGrid, ArrowLeft, MessageCircle, FileText, UserRound, Plus } from "../components/ui/icons";
import { ThreadError } from "./ThreadError";
import { ThreadActions } from "./ThreadActions";
import { SettingsSheet } from "../components/settings/SettingsSheet";
import { ProcessesSheet } from "../components/processes/ProcessesSheet";
import { CanvasRail, CanvasSheet } from "../components/views/CanvasSurface";
import { ConnectionPill } from "../components/ConnectionPill";
import { clearComposerPrefill, openProcesses, openSettings, state } from "../store/store";
import { EventCardRenderer } from "../views/EventCardRenderer";
import { getClient } from "../ws/client";
import { ThreadNavigation } from "./ThreadNavigation";
import { threadNavigationOpen as navigationOpen, setThreadNavigationOpen as setNavigationOpen } from "./navigation";
import { artifactState } from "../artifacts/store";
import { focusThread, openThread, retryThreadMessage, sendThreadMessage, threadAction, threadState } from "./store";

const button = "inline-flex min-h-11 items-center justify-center gap-2 rounded-lg px-3 py-2 text-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-50";
const iconButton = "inline-flex size-11 shrink-0 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring aria-pressed:bg-muted aria-pressed:text-foreground";
function ThreadConversation(props: { id: number; attachments: AttachmentsController; globalArtifacts: boolean; onConversation: () => void; onHome: () => void }) {
  const [artifactView, setArtifactView] = createSignal(false);
  const showArtifacts = () => artifactView() || props.globalArtifacts;
  const attachments = props.attachments;
  const current = () => threadState.threads.find(t => t.id === props.id);
  const history = () => threadState.histories[props.id];
  const messages = () => history()?.messages ?? [];
  const stream = createMemo(() => splitStreamingReply(threadState.streams[props.id] ?? []));
  const pending = () => threadState.pending.filter(p => p.threadId === props.id);
  const thinking = () => history()?.turns.some(t => t.state === "running" || t.state === "queued") ?? false;
  const [loading, setLoading] = createSignal(false);
  let scroller: HTMLDivElement | undefined;
  let following = true;
  createEffect(() => messages().length + pending().length + (threadState.streams[props.id] ?? []).length, (count) => {
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
  const artifactCount = () => artifactState.summaries.filter(artifact => artifact.thread_ids.includes(props.id)).length;
  return <main class={["flex min-h-0 min-w-0 flex-1 flex-col", props.id !== 0 ? "thread-focus-frame rounded-xl border border-border bg-background" : ""]} data-thread-id={props.id} aria-label={props.id === 0 ? "Home conversation" : current()?.title ?? "Thread conversation"}>
    <Show when={props.id !== 0 || showArtifacts()}>
      <header class="flex min-h-14 shrink-0 items-center gap-0.5 border-b border-border/60 px-1.5 sm:gap-1 sm:px-2" data-slot="thread-context">
        <button class={iconButton} aria-label={props.globalArtifacts ? "Back to conversation" : "Back to Hirsel"} title={props.globalArtifacts ? "Back to conversation" : "Back to Hirsel"} onClick={() => { if (props.globalArtifacts) props.onConversation(); else props.onHome(); }}><ArrowLeft class="size-4" /></button>
        <GitBranch class="hidden size-4 shrink-0 text-muted-foreground sm:block" />
        <h1 class="min-w-0 flex-1 text-sm font-medium"><Show when={props.id !== 0} fallback={<span class="px-1">Home</span>}><button class="block min-h-11 w-full truncate rounded-lg px-1 text-left hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" title="Show full thread name in Threads" aria-haspopup="dialog" aria-controls="thread-navigation" onClick={() => setNavigationOpen(true)}>{current()?.title ?? "Loading thread…"}</button></Show></h1>
        <Show when={current()?.attention === "needs_owner"}><span class="size-2 shrink-0 rounded-full bg-status-attention" role="status" aria-label="Needs you" title="Needs you" /></Show>
        <Show when={current()?.settled_at}><span class="hidden text-xs text-muted-foreground sm:inline">Settled</span></Show>
        <button class={iconButton} aria-label="Conversation" title="Conversation" aria-pressed={!showArtifacts() ? "true" : "false"} onClick={() => { setArtifactView(false); props.onConversation(); }}><MessageCircle class="size-4" /></button>
        <button class={`${iconButton} relative`} aria-label="Artifacts" title="Thread artifacts" aria-pressed={artifactView() && !props.globalArtifacts ? "true" : "false"} onClick={() => { props.onConversation(); setArtifactView(true); }}><FileText class="size-4" /><Show when={artifactCount() > 0}><span aria-hidden="true" class="absolute top-0.5 right-0.5 grid min-w-3.5 place-items-center rounded-full bg-muted px-0.5 text-[10px] tabular-nums">{artifactCount()}</span></Show></button>
        <Show when={props.id !== 0 && current()}>
          <ThreadActions thread={current()!} />
        </Show>
      </header>
    </Show>
    <div ref={node => { scroller = node; }} class="min-h-0 flex-1 overflow-y-auto px-3 py-6 sm:px-gutter" data-slot="thread-scroll" onScroll={() => { if (scroller) following = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 80; }}>
      <Show when={!showArtifacts()} fallback={<ArtifactList threadId={props.globalArtifacts ? undefined : props.id} onResume={props.onConversation} />}>
      <div class="mx-auto flex w-full max-w-measure flex-col gap-6">
        <Show when={current()?.description}><p class="text-sm text-muted-foreground">{current()?.description}</p></Show>
        <Show when={current()?.instrument && Object.keys(current()!.instrument!).length > 0}>
          <Show when={current()?.revision} keyed>{revision => <EventCardRenderer ui={current()?.instrument ?? undefined} onAction={(action, data) => threadAction(props.id, action, data, revision)} />}</Show>
        </Show>
        <Show when={history()?.hasMore}><button class={button} disabled={loading()} onClick={() => void earlier()}>{loading() ? "Loading…" : "Load earlier messages"}</button></Show>
        <Show when={!history()?.loaded && !(threadState.error?.operation === "load" && threadState.error.threadId === props.id)}><p role="status" class="text-sm text-muted-foreground">Loading conversation…</p></Show>
        <Show when={history()?.loaded && messages().length === 0 && pending().length === 0 && !thinking()}><p class="text-sm text-muted-foreground">{props.id === 0 ? "What would you like to work on?" : "Start the conversation for this thread."}</p></Show>
        <For each={messages()}>{message => <article data-message-id={message.id} aria-label={message.author === "owner" ? "You" : "Hirsel"} class={["flex items-start gap-3", message.author === "owner" ? "flex-row-reverse" : ""]}>
          <span class="grid size-8 shrink-0 place-items-center rounded-full bg-muted/45" aria-hidden="true"><Show when={message.author === "owner"} fallback={<BrandMark size={22} />}><UserRound class="size-4 text-muted-foreground" /></Show></span>
          <div class={message.author === "owner" ? "min-w-0 max-w-[85%] rounded-xl bg-muted/65 px-4 py-3" : "min-w-0 flex-1 pt-1"}>
          <Markdown>{message.body}</Markdown>
          <For each={message.artifact_ids ?? []}>{id => <ArtifactCard id={id} />}</For>
          <Show when={message.attachments?.length}><ul class="mt-2 text-xs text-muted-foreground"><For each={message.attachments}>{blob => <li><button class="underline" onClick={() => { void getClient()?.getBlobUrl(blob.id).then(url => window.open(url, "_blank", "noopener,noreferrer")); }}>{blob.name}</button></li>}</For></ul></Show>
          </div>
        </article>}</For>
        <For each={pending()}>{message => <article class="ml-6 rounded-xl bg-muted/65 px-4 py-3 opacity-70"><Markdown>{message.body}</Markdown><span class="text-xs">{message.failed ? "Failed to send" : state.connection === "connected" ? "Sending…" : "Waiting for connection…"}</span><Show when={message.failed}><button class={button} onClick={() => retryThreadMessage(message.clientId)}>Retry</button></Show></article>}</For>
        <Show when={stream().reply}><div data-slot="streaming-reply"><Markdown>{stream().reply}</Markdown></div></Show>
        <For each={history()?.activities.filter(ownerFacingActivity)}>{activity => <article data-activity-id={activity.id} class="space-y-2"><p class="text-xs font-medium text-muted-foreground">Hirsel · <time datetime={activity.ts}>{new Date(activity.ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</time></p><Markdown>{activityText(activity)}</Markdown></article>}</For>
        <ThreadExecution id={props.id} liveEvents={stream().activity} />
        <Show when={thinking()}><p role="status" class="text-sm text-muted-foreground">Hirsel is working…</p></Show>
      </div>
      </Show>
    </div>
    <ThreadError threadId={props.id} />
    <Composer ariaLabel={props.id === 0 ? "Message Hirsel" : `Message ${current()?.title ?? "this Thread"}`} prefill={state.composerPrefill} onConsumePrefill={clearComposerPrefill} draftKey={`thread-${props.id}`} attachments={attachments} thinking={thinking()} focused={props.id !== 0} tasks={threadState.threads}
      onSend={(body, _ref, mode, blobs, mentions) => {
        sendThreadMessage(props.id, body, mode, blobs, mentions);
      }}
      onStop={() => getClient()?.cancelTurn(props.id)} getLastOwnerBody={() => messages().findLast(m => m.author === "owner")?.body ?? null} />
  </main>;
}

export function ThreadShell() {
  const [globalArtifacts, setGlobalArtifacts] = createSignal(false);
  onCleanup(() => setNavigationOpen(false));
  const selectThread = (id: number) => { setGlobalArtifacts(false); setNavigationOpen(false); focusThread(id); };
  const composers = new Map<number, { attachments: AttachmentsController; dispose: () => void }>();
  const attachmentsFor = (id: number) => {
    if (!composers.has(id)) composers.set(id, createRoot(dispose => ({ attachments: createComposerAttachments(), dispose })));
    return composers.get(id)!.attachments;
  };
  onCleanup(() => { for (const composer of composers.values()) composer.dispose(); });
  const onPop = () => { const match = /^\/t\/(\d+)$/.exec(location.pathname); focusThread(match ? Number(match[1]) : 0, false); };
  window.addEventListener("popstate", onPop);
  onCleanup(() => window.removeEventListener("popstate", onPop));
  createEffect(() => { const thread = threadState.threads.find(t => t.id === threadState.focusedId); return thread && !thread.read && state.connection === "connected" ? thread.id : null; }, (id) => { if (id !== null) threadAction(id, "read"); });
  return <div class="flex h-dvh min-h-0 bg-background text-foreground">
    <nav aria-label="Hirsel" data-slot="icon-rail" class="relative z-10 flex w-14 shrink-0 flex-col items-center gap-2 py-2">
      <button class={iconButton} aria-label="Home" title="Home" aria-pressed={threadState.focusedId === 0 && !globalArtifacts() ? "true" : "false"} onClick={() => selectThread(0)}><BrandMark size={23} /></button>
      <button class={iconButton} aria-label="Threads" title="Threads" data-slot="thread-navigation-trigger" aria-controls="thread-navigation" aria-expanded={navigationOpen() ? "true" : "false"} aria-pressed={threadState.focusedId !== 0 && !globalArtifacts() ? "true" : "false"} onClick={() => setNavigationOpen(value => !value)}><GitBranch class="size-5" /></button>
      <Show when={threadState.focusedId !== 0 && !globalArtifacts()}><svg class="pointer-events-none absolute top-[58px] left-12 h-8 w-4 text-border" viewBox="0 0 16 32" fill="none" aria-hidden="true" data-slot="thread-connector"><path d="M0 24h4c8 0 12-4 12-12V0" stroke="currentColor" /></svg></Show>
      <button class={iconButton} aria-label="New thread" title="New thread" onClick={() => { setNavigationOpen(true); requestAnimationFrame(() => { if (navigationOpen()) document.querySelector<HTMLInputElement>('#thread-navigation input')?.focus(); }); }}><Plus class="size-5" /></button>
      <button class={iconButton} aria-label="All artifacts" title="All artifacts" aria-pressed={globalArtifacts() ? "true" : "false"} onClick={() => setGlobalArtifacts(value => !value)}><LayoutGrid class="size-5" /></button>
      <button class={iconButton} aria-label="Processes" title="Processes" onClick={openProcesses}><Activity class="size-5" /></button>
      <div class="flex-1" />
      <button class={iconButton} aria-label="Settings" title="Settings" onClick={() => openSettings()}><Settings class="size-5" /></button>
    </nav>
    <ThreadNavigation open={navigationOpen()} onClose={() => setNavigationOpen(false)} onSelect={selectThread} />
    <div class="flex min-h-0 min-w-0 flex-1 flex-col">
      <Show when={state.connection !== "connected"}><div class="flex shrink-0 justify-end px-3 pt-2"><ConnectionPill /></div></Show>
      <div class="flex min-h-0 flex-1 gap-2 py-2 pr-2 pl-2 sm:gap-3 sm:pr-3">
        <Show when={{ id: threadState.focusedId }} keyed>{focused => <ThreadConversation id={focused.id} attachments={attachmentsFor(focused.id)} globalArtifacts={globalArtifacts()} onConversation={() => setGlobalArtifacts(false)} onHome={() => selectThread(0)} />}</Show>
        <ArtifactSurface />
        <CanvasRail /><CanvasSheet /><ProcessesSheet /><SettingsSheet />
      </div>
    </div>
  </div>;
}
