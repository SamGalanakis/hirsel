import { ShowcaseButton, ShowcaseSurface } from "../artifacts/ShowcaseSurface";
import { ReachStrip } from "../grants/ReachStrip";
import { RelatedContext } from "../related/context";
import { RelatedList } from "../related/RelatedList";
import { relatedState } from "../related/store";
import { anyOverlayOpen, createMediaFlag } from "../lib/focus";
import { consumeDraftArtifact, draftArtifact, stageDraftArtifact } from "../artifacts/draft-context";
import { historyId } from "../lib/history";
import { ArtifactCard, ArtifactList, ArtifactSurface } from "../artifacts/ArtifactSurface";
import { createEffect, createMemo, createRoot, createSignal, For, onCleanup, onSettled, Show } from "solid-js";

import { Markdown } from "../components/Markdown";
import { Composer } from "../components/chat/Composer";
import { createComposerAttachments, type AttachmentsController } from "../components/chat/useAttachments";
import { ThreadMessage } from "./ThreadMessages";
import { ConversationNote } from "./ThreadWork";
import { ThreadCreate } from "./ThreadCreate";
import { openThreadCreate } from "./create";
import { quietWakeTurn } from "./work-summary";
import { conversationEntries, type ConversationEntry } from "./conversation";
import { emptyHistory } from "./model";
import { BrandMark } from "../components/BrandMark";
import { Activity, Settings, GitBranch, Info, LayoutGrid, ArrowLeft, MessageCircle, FileText, Plus } from "../components/ui/icons";
import { ThreadLink } from "./ThreadRef";
import { threadAncestors } from "./tree";
import { attentionQueue, attentionThreads } from "./attention";
import { elapsedTime } from "./status";
import { ThreadError } from "./ThreadError";
import { ThreadAvatar } from "./ThreadAvatar";
import { ThreadIconPicker } from "./ThreadIconPicker";
import { ThreadInfo } from "./ThreadInfo";
import { ThreadActions } from "./ThreadActions";
import { SettingsSheet } from "../components/settings/SettingsSheet";
import { ProcessesSheet } from "../components/processes/ProcessesSheet";
import { CanvasRail, CanvasSheet, CanvasButton } from "../components/views/CanvasSurface";
import { ConnectionPill } from "../components/ConnectionPill";
import { closeRightRegion, openProcesses, openSettings, state } from "../store/store";
import { ThreadInstrument } from "../views/ThreadInstrument";
import { getClient } from "../ws/client";
import { ThreadNavigation, type ThreadNavigationMode } from "./ThreadNavigation";
import { threadNavigationOpen as navigationOpen, threadNavigationIntent, openThreadNavigation, closeThreadNavigation, popThreadVisit, previousThread, recordThreadVisit } from "./navigation";
import { artifactState } from "../artifacts/store";
import type { Thread } from "./types";
import { focusThread, followThreadLocation, openThread, retryThreadMessage, sendThreadMessage, threadAction, threadState } from "./store";

const button = "inline-flex min-h-11 items-center justify-center gap-2 rounded-lg px-3 py-2 text-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-50";
const iconButton = "inline-flex size-11 shrink-0 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring aria-pressed:bg-muted aria-pressed:text-foreground";
/** A rail icon at `split` and above; a labelled bottom-bar destination below
 * it. One control, two shapes — the phone never loses the label. */
const barButton = "inline-flex min-h-12 flex-1 flex-col items-center justify-center gap-0.5 rounded-lg px-1 py-1 text-[10px] font-medium text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring aria-pressed:text-foreground split:size-11 split:min-h-11 split:flex-none split:gap-0 split:p-0 split:text-sm split:aria-pressed:bg-muted split:aria-pressed:text-foreground";
const desktopNavigationPreferenceKey = "hirsel.thread-navigation.desktop";
function readDesktopNavigationPreference(): boolean {
  try { return localStorage.getItem(desktopNavigationPreferenceKey) !== "closed"; }
  catch { return true; }
}
function writeDesktopNavigationPreference(open: boolean): void {
  try { localStorage.setItem(desktopNavigationPreferenceKey, open ? "open" : "closed"); }
  catch { /* The current session still responds when storage is unavailable. */ }
}
/** "Needs you" is a word, not a colour: the header wears a labelled pill with
 * how long this Thread has been waiting, in the one attention token. */
function NeedsYouPill(props: { thread: { attention: string; last_activity_at: string } }) {
  const [now, setNow] = createSignal(Date.now());
  const timer = setInterval(() => setNow(Date.now()), 30_000);
  onCleanup(() => clearInterval(timer));
  const waited = () => elapsedTime(props.thread.last_activity_at, now());
  return <Show when={props.thread.attention === "needs_owner"}>
    {/* On a phone the words take the whole second line rather than eating the
        Thread's name: the header wraps, the pill goes last, nothing overflows. */}
    <span class="order-last inline-flex w-full shrink-0 items-center gap-1 self-start rounded-full border border-status-attention/40 bg-status-attention/10 px-2 py-0.5 text-meta font-medium text-status-attention split:order-none split:w-auto split:self-auto" role="status" data-slot="needs-you-pill" title="This Thread is waiting on you">
      <span aria-hidden="true" class="size-1.5 rounded-full bg-status-attention" />Needs you<Show when={waited()}>{age => <span aria-hidden="true" class="tabular-nums">· {age()}</span>}</Show>
    </span>
  </Show>;
}
function ThreadConversation(props: { id: number; historyId: string; attachments: AttachmentsController; globalArtifacts: boolean; onConversation: () => void }) {
  const [pane, setPane] = createSignal<"conversation" | "related" | "info">("conversation");
  const showRelated = () => pane() === "related" || props.globalArtifacts;
  /** The Thread's own facts, in the frame, in place of the conversation. */
  const showInfo = () => pane() === "info" && !props.globalArtifacts;
  /** A pane of this Thread standing in place of its conversation. The global
   * artifact browser is not one: it is a layer over the addressed Thread and
   * keeps its destination, so it keeps its composer too. */
  const threadPane = () => showInfo() || (pane() === "related" && !props.globalArtifacts);
  /** Info and Related end at their content — no composer, no bottom chrome. */
  const writable = () => !threadPane();
  /** Back has one meaning — go back. A pane or browser standing in for the
   * conversation returns to it; otherwise the Thread this session came from;
   * otherwise the overview. It never opens the inventory: the rail and the
   * phone bar own that, and with the column docked opening it does nothing. */
  const backsToConversation = () => threadPane() || props.globalArtifacts;
  const backTitle = () => {
    if (backsToConversation()) return "Back to conversation";
    const previous = previousThread();
    return previous === null ? "Back to overview" : `Back to #${previous}`;
  };
  const goBack = () => {
    if (backsToConversation()) { setPane("conversation"); props.onConversation(); return; }
    focusThread(popThreadVisit());
  };
  const attachments = props.attachments;
  const current = () => threadState.threads.find(t => t.id === props.id);
  const history = () => threadState.histories[props.id];
  const messages = () => history()?.messages ?? [];
  const entries = createMemo(() => conversationEntries(history() ?? emptyHistory()));
  /** A wake that produced nothing to read is not a card. Consecutive ones fold
   * into one quiet note so a Space driven by child reports reads as a
   * conversation rather than a stack of empty activity boxes. */
  const rendered = createMemo(() => {
    const rows: ({ key: string; entry: ConversationEntry } | { key: string; quiet: number })[] = [];
    for (const entry of entries()) {
      const turn = entry.kind === "turn" ? entry.turn : null;
      const quiet = turn !== null && quietWakeTurn(turn, (history()?.activities ?? []).filter(activity => activity.turn_id === turn.id), threadState.turnDetails[turn.id] ?? []);
      const last = rows[rows.length - 1];
      if (quiet && last && "quiet" in last) rows[rows.length - 1] = { key: last.key, quiet: last.quiet + 1 };
      else if (quiet) rows.push({ key: `quiet-${entry.key}`, quiet: 1 });
      else rows.push({ key: entry.key, entry });
    }
    return rows;
  });
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
  return <RelatedContext value={origin}><main class="thread-focus-frame flex min-h-0 w-full min-w-0 flex-1 flex-col rounded-xl border border-border bg-background split:min-w-[26rem]" data-thread-id={props.id} aria-label={current()?.title ?? "Thread conversation"}>
      <header class="flex min-h-14 shrink-0 flex-wrap items-center gap-0.5 border-b border-border/60 px-1.5 pb-1 sm:gap-1 sm:px-2" data-slot="thread-context">
        <button class={iconButton} aria-label="Back" title={backTitle()} onClick={goBack}><ArrowLeft class="size-4" /></button>
        <ThreadAvatar thread={{ id: props.id, kind: current()?.kind ?? "space", title: current()?.title ?? "Thread", icon: current()?.icon }} />
        <h1 class="min-w-0 flex-1 text-sm font-medium"><button class="block min-h-11 w-full truncate rounded-lg px-1 text-left hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" aria-label={current()?.title ?? "Loading thread…"} title="Show full name in Spaces and Tasks" aria-controls="thread-navigation" onClick={() => openThreadNavigation()}><span class="flex min-w-0 items-center gap-1.5"><span class="shrink-0 whitespace-nowrap text-xs tabular-nums text-muted-foreground">#{props.id}</span><span class="truncate">{current()?.title ?? "Loading thread…"}</span></span></button></h1>
        <Show when={current()}>{thread => <NeedsYouPill thread={thread()} />}</Show>
        <Show when={current()}>{thread => <span class="hidden text-xs capitalize text-muted-foreground sm:inline">{thread().kind}{thread().kind === "task" && thread().settled_at ? " · Done" : ""}</span>}</Show>
        <div role="tablist" aria-label="Thread views" class="flex shrink-0 items-center" data-slot="thread-views">
          <button role="tab" class={iconButton} aria-label="Conversation" title="Conversation" aria-selected={!showRelated() && !showInfo() ? "true" : "false"} tabindex={!showRelated() && !showInfo() ? 0 : -1} onClick={() => { setPane("conversation"); props.onConversation(); }}><MessageCircle class="size-4" /></button>
          <button role="tab" class={iconButton} aria-label="Info" title="About this Thread" aria-selected={showInfo() ? "true" : "false"} tabindex={showInfo() ? 0 : -1} onClick={() => { props.onConversation(); setPane("info"); }}><Info class="size-4" /></button>
          <button role="tab" class={`${iconButton} relative`} aria-label="Related" title="Related links and artifacts" aria-selected={pane() === "related" && !props.globalArtifacts ? "true" : "false"} tabindex={pane() === "related" && !props.globalArtifacts ? 0 : -1} onClick={() => { props.onConversation(); setPane("related"); }}><FileText class="size-4" /><Show when={relatedCount() > 0}><span aria-hidden="true" class="absolute top-0.5 right-0.5 grid min-w-3.5 place-items-center rounded-full bg-muted px-0.5 text-[10px] tabular-nums">{relatedCount()}</span></Show></button>
        </div>
        <ShowcaseButton threadId={props.id} />
        <CanvasButton />
        <Show when={current()}>
          <ThreadActions thread={current()!} />
        </Show>
      </header>
    <div ref={node => { scroller = node; }} class="min-h-0 flex-1 overflow-y-auto px-4 py-6 sm:px-gutter" data-slot="thread-scroll" onScroll={() => { if (scroller) following = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 80; }}>
      <Show when={!showRelated()} fallback={<Show when={props.globalArtifacts} fallback={<RelatedList origin={origin} />}><ArtifactList onResume={props.onConversation} /></Show>}>
      <Show when={showInfo() && current()} fallback={
      <div class="mx-auto flex w-full max-w-measure flex-col gap-6">
        <Show when={current()?.parent_thread_id !== null && current()?.parent_thread_id !== undefined}><nav aria-label="Thread ancestry" class="flex flex-wrap items-center gap-1 text-xs text-muted-foreground"><For each={threadAncestors(threadState.threads, props.id)}>{parent => <><ThreadLink id={parent.id} /><span aria-hidden="true">/</span></>}</For><span class="break-words">#{props.id} {current()?.title}</span></nav></Show>
        <Show when={current()?.instrument}>
          {/* Keyed on the instrument itself, not the revision: an unrelated
              revision bump (a message, a read receipt) must not remount the
              card and discard what the Owner has typed. The action carries the
              revision read at submit time, which the Host checks exactly. */}
          <Show when={JSON.stringify(current()?.instrument)} keyed>{spec => <ThreadInstrument ui={JSON.parse(spec) as Thread["instrument"] ?? undefined} allowSettlement={current()?.kind === "task"} onAction={(action, data) => threadAction(props.historyId, props.id, action, data, current()?.revision)} />}</Show>
        </Show>
        <Show when={history()?.hasMore}><button class={button} disabled={loading()} onClick={() => void earlier()}>{loading() ? "Loading…" : "Load earlier messages"}</button></Show>
        <Show when={!history()?.loaded && !(threadState.error?.operation === "load" && threadState.error.threadId === props.id)}><p role="status" class="text-sm text-muted-foreground">Loading conversation…</p></Show>
        <Show when={history()?.loaded && messages().length === 0 && pending().length === 0 && !thinking()}><p class="text-sm text-muted-foreground">Start the conversation for this thread.</p></Show>
        <For each={rendered()} keyed={row => row.key}>{row => <Show when={"entry" in row() ? row() as { entry: ConversationEntry } : undefined} fallback={<ConversationNote title="Turns that woke this Thread and left nothing to show">{(row() as { quiet: number }).quiet} quiet {(row() as { quiet: number }).quiet === 1 ? "wake" : "wakes"}</ConversationNote>}>{owned => <ThreadMessage entry={owned().entry} history={history() ?? emptyHistory()} threadId={props.id} />}</Show>}</For>
        <For each={pending()} keyed={message => message.clientId}>{message => <PendingMessageRow message={message()} />}</For>

      </div>}>{thread => <ThreadInfo thread={thread()} historyId={props.historyId} onRelated={() => setPane("related")} />}</Show>
      </Show>
    </div>
    <ThreadError threadId={props.id} />
    <Show when={!showRelated() && !showInfo()}><ReachStrip origin={origin} /></Show>
    <Show when={writable()}>
    <Composer artifactContext={draftArtifact(props.id)} onRemoveArtifactContext={() => stageDraftArtifact(props.id, null)} onConsumeArtifactContext={id => consumeDraftArtifact(props.id, id)} ariaLabel={`Message ${current()?.title ?? "this Thread"}`} shortLabel={`Message #${props.id}`} draftKey={`${historyId()}:thread-${props.id}`} attachments={attachments} thinking={thinking()} focused threads={threadState.threads}
      onSend={(body, mode, blobs, mentions, artifactIds) => {
        sendThreadMessage(props.historyId, props.id, body, mode, blobs, mentions, artifactIds);
      }}
      onStop={() => getClient()?.cancelTurn(props.historyId, props.id)} getLastOwnerBody={() => messages().findLast(m => m.author === "owner")?.body ?? null} />
    </Show>
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
  return <article data-author="owner" class={`ml-auto max-w-[85%] rounded-xl rounded-br-sm bg-primary px-3.5 py-2.5 text-primary-foreground [&_code]:bg-current/10 sm:max-w-[60%] ${props.message.failed ? "" : "opacity-70"}`}><Markdown>{props.message.body}</Markdown><For each={props.message.artifactIds}>{id => <ArtifactCard id={id} />}</For><span class="text-xs">{props.message.failed ? "Failed to send" : state.connection === "connected" ? "Sending…" : "Waiting for connection…"}</span><Show when={props.message.failed}><button class={button} ref={node => { retryButton = node; }} onFocus={() => { ownsFocus = true; }} onClick={() => retryThreadMessage(props.message.clientId)}>Retry</button></Show></article>;
}

/** One row of the overview queue: who is waiting, for how long, and — when it
 * is the Owner they are waiting for — the actual question. */
function QueueRow(props: { entry: ReturnType<typeof attentionQueue>[number]; onSelect: (id: number) => void }) {
  const thread = () => props.entry.thread;
  return <li>
    <button type="button" data-queue-thread={thread().id} data-queue-group={props.entry.group}
      class="flex min-h-11 w-full min-w-0 items-start gap-2.5 rounded-lg px-2 py-2.5 text-left transition-colors hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      onClick={() => props.onSelect(thread().id)}>
      <span class="mt-0.5 shrink-0"><ThreadAvatar thread={thread()} dense /></span>
      <span class="min-w-0 flex-1">
        <span class="flex min-w-0 items-center gap-2">
          <span class="min-w-0 flex-1 truncate text-sm font-medium text-foreground">{thread().title}</span>
          <span class={`shrink-0 whitespace-nowrap text-meta tabular-nums ${props.entry.group === "attention" ? "text-status-attention" : props.entry.group === "running" ? "text-status-active" : "text-muted-foreground"}`}>{props.entry.group === "running" ? "running" : props.entry.waited}</span>
        </span>
        <Show when={props.entry.excerpt}><span class="mt-0.5 line-clamp-2 block text-sm text-muted-foreground">{props.entry.excerpt}</span></Show>
      </span>
    </button>
  </li>;
}
/**
 * The unaddressed surface is the attention queue, not an empty field: the
 * Threads waiting on the Owner first with the question they asked, then what is
 * running, then what was active last. "Choose a Space or Task" survives only
 * for an account that has none. The same view serves phone and desktop.
 */
function ThreadStart(props: { globalArtifacts: boolean; browsable: boolean; onSelect: (id: number) => void }) {
  const [now, setNow] = createSignal(Date.now());
  const timer = setInterval(() => setNow(Date.now()), 30_000);
  onCleanup(() => clearInterval(timer));
  const queue = createMemo(() => attentionQueue(threadState.threads, threadState.histories, now(), state.connection === "connected"));
  const grouped = createMemo(() => [
    { id: "attention", label: `Needs you (${queue().filter(entry => entry.group === "attention").length})`, entries: queue().filter(entry => entry.group === "attention") },
    { id: "running", label: "Running", entries: queue().filter(entry => entry.group === "running") },
    { id: "recent", label: "Recently active", entries: queue().filter(entry => entry.group === "recent") },
  ].filter(group => group.entries.length > 0));
  return <main data-slot="thread-empty" class="flex min-h-0 min-w-0 flex-1 flex-col overflow-y-auto p-5 sm:p-8">
    <Show when={props.globalArtifacts} fallback={
      <Show when={threadState.linkError} fallback={
        <Show when={threadState.threads.length > 0 && threadState.focusedId === null} fallback={<div class="m-auto w-full max-w-md space-y-4">
          <h1 class="text-lg font-medium">{threadState.focusedId !== null ? `Thread #${threadState.focusedId} is unavailable` : "Start with a Space or Task"}</h1>
          <p class="text-sm text-muted-foreground">{threadState.focusedId !== null ? "It may have been removed, or it belongs to another history." : "Spaces hold ongoing context. Tasks hold work you can mark done."}</p>
          <Show when={threadState.threads.length > 0 && props.browsable}><button class={button} onClick={() => openThreadNavigation()}>Browse Spaces &amp; Tasks</button></Show>
          <button class={`${button} bg-primary text-primary-foreground hover:bg-primary/90 hover:text-primary-foreground`} disabled={state.connection !== "connected"} onClick={() => openThreadCreate(null)}><Plus class="size-4" />Start a Space or Task</button>
        </div>}>
        <div class="mx-auto flex w-full max-w-measure flex-col gap-5" data-slot="attention-queue">
          <div class="flex flex-wrap items-center justify-between gap-2">
            <h1 class="text-lg font-medium">What needs you</h1>
            <div class="flex items-center gap-1">
              <Show when={props.browsable}><button class={button} onClick={() => openThreadNavigation()}>Browse Spaces &amp; Tasks</button></Show>
              <button class={`${button} bg-primary text-primary-foreground hover:bg-primary/90 hover:text-primary-foreground`} disabled={state.connection !== "connected"} onClick={() => openThreadCreate(null)}><Plus class="size-4" />New</button>
              {/* Phone reaches Settings from the overview: there is no icon rail. */}
              <button class={`${button} min-w-11 split:hidden`} aria-label="Settings" title="Settings" onClick={() => openSettings()}><Settings class="size-4" /></button>
            </div>
          </div>
          <Show when={grouped().length > 0} fallback={<p class="text-sm text-muted-foreground">Nothing is waiting on you. Open a Space or Task, or start a new one.</p>}>
            <For each={grouped()}>{group => <section class="flex flex-col gap-1" aria-label={group.label}>
              <h2 class="px-2 text-meta font-medium uppercase tracking-wider text-muted-foreground">{group.label}</h2>
              <ul class="flex flex-col"><For each={group.entries}>{entry => <QueueRow entry={entry} onSelect={props.onSelect} />}</For></ul>
            </section>}</For>
          </Show>
        </div></Show>
      }><div class="m-auto w-full max-w-md space-y-4">
        <h1 class="text-lg font-medium">{threadState.linkError}</h1>
        <p class="text-sm text-muted-foreground">Your drafts are kept. Choose a conversation from this history to continue.</p>
        <button class={button} onClick={() => { focusThread(null); openThreadNavigation(); }}>Return to Spaces &amp; Tasks</button>
      </div></Show>
    }><ArtifactList /></Show>
  </main>;
}
export function ThreadShell() {
  /** `rail` (1100) is where the inventory can stand beside a readable
   * conversation, so that — not `workspace` — is the dock threshold. Between
   * `split` and `rail` the column collapses to its 56px icon strip rather than
   * vanishing, and below `split` the phone has no left rail at all: the bottom
   * bar is the way around. */
  const dockable = createMediaFlag("(min-width: 1100px)");
  const phone = createMediaFlag("(max-width: 899.98px)");
  /** Four standing panes (inventory, conversation, showcase/artifact, utility)
   * only fit past this. Below it the utility region and the showcase take
   * turns, so the conversation keeps its 26rem reading minimum. */
  const fourPanes = createMediaFlag("(min-width: 1600px)");
  const [desktopNavigationOpen, setDesktopNavigationOpen] = createSignal(readDesktopNavigationPreference());
  const navigationMode = (): ThreadNavigationMode => {
    if (phone()) return navigationOpen() ? "modal" : "hidden";
    if (dockable() && desktopNavigationOpen()) return "docked";
    return navigationOpen() ? "modal" : "compact";
  };
  const setDesktopNavigation = (open: boolean) => {
    setDesktopNavigationOpen(open);
    writeDesktopNavigationPreference(open);
  };
  const openNavigation = (intent: Parameters<typeof openThreadNavigation>[0] = { kind: "browse" }) => {
    if (dockable()) setDesktopNavigation(true);
    openThreadNavigation(intent);
  };
  const closeNavigation = () => {
    if (dockable()) setDesktopNavigation(false);
    closeThreadNavigation();
  };
  createEffect(() => ({ wide: dockable(), intent: threadNavigationIntent() }), ({ wide, intent }) => {
    if (wide && intent) setDesktopNavigation(true);
  });
  const [globalArtifacts, setGlobalArtifacts] = createSignal(false);
  const [attentionNow, setAttentionNow] = createSignal(Date.now());
  const attentionCount = createMemo(() => attentionThreads(threadState.threads, attentionNow()).length);
  createEffect(() => ({ now: attentionNow(), deadlines: threadState.threads.filter(thread => !thread.archived_at && thread.attention === "needs_owner").map(thread => Date.parse(thread.snoozed_until ?? "")) }), ({ now, deadlines }) => {
    const next = Math.min(...deadlines.filter(time => time > now));
    if (!Number.isFinite(next)) return;
    const timer = setTimeout(() => setAttentionNow(Date.now()), Math.min(next - now + 1, 2_147_483_647));
    return () => clearTimeout(timer);
  });
  /** The utility region and the showcase are exclusive below `fourPanes`: the
   * one that loses the slot collapses to a tab that restores it, never to an
   * empty strip or a 110px conversation. */
  const sideCollapsed = () => state.rightRegion !== "none" && dockable() && !fourPanes();
  const showcasedThread = () => threadState.threads.find(thread => thread.id === threadState.focusedId);
  const collapsedShowcase = () => sideCollapsed() && showcasedThread()?.showcased_artifact_id != null;
  createEffect(() => sideCollapsed() && artifactState.preview.status !== "idle", crowded => { if (crowded) closeRightRegion(); });
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
  /** Back needs a trail, and every way into a Thread — the drawer, a link, the
   * queue, the browser's own Back — ends here. */
  createEffect(() => threadState.focusedId, id => recordThreadVisit(id));
  const onPop = () => followThreadLocation();
  window.addEventListener("popstate", onPop);
  onCleanup(() => window.removeEventListener("popstate", onPop));
  createEffect(() => { const thread = threadState.threads.find(t => t.id === threadState.focusedId); const history = historyId(); return thread && history && !thread.read && state.connection === "connected" ? { id: thread.id, history } : null; }, (target) => { if (target) threadAction(target.history, target.id, "read"); });
  return <div class="flex h-dvh min-h-0 flex-col bg-background text-foreground split:flex-row">
    {/* One set of destinations, two shapes: a 56px icon rail beside the work at
        `split` and above, a labelled bottom bar below it. There is no top-left
        rail on a phone — the bar is the reach. */}
    <nav aria-label="Hirsel" data-slot="icon-rail" class="relative z-10 order-last flex w-full shrink-0 items-stretch justify-around gap-0.5 border-t border-border px-1 pb-[env(safe-area-inset-bottom)] pt-1 split:order-none split:w-14 split:flex-col split:items-center split:justify-start split:gap-2 split:border-t-0 split:px-0 split:py-2">
      <button class={`${iconButton} hidden split:inline-flex`} aria-label="Thread overview" title="Thread overview" aria-pressed={threadState.focusedId === null && !globalArtifacts() ? "true" : "false"} onClick={() => { setGlobalArtifacts(false); focusThread(null); }}><BrandMark size={23} /></button>
      <button class={`${barButton} relative`} aria-label="Spaces and Tasks" aria-describedby={attentionCount() > 0 ? "thread-attention-summary" : undefined} title={attentionCount() > 0 ? `Spaces & Tasks · ${attentionCount()} need you` : "Spaces & Tasks"} data-slot="thread-navigation-trigger" aria-controls="thread-navigation" aria-expanded={navigationMode() === "docked" || navigationMode() === "modal" ? "true" : "false"} aria-pressed={threadState.focusedId !== null && !globalArtifacts() ? "true" : "false"} onClick={() => navigationMode() === "docked" || navigationMode() === "modal" ? closeNavigation() : openNavigation()}><GitBranch class="size-5" /><span class="split:hidden">Threads</span><Show when={attentionCount() > 0}><span aria-hidden="true" class="absolute right-2 top-1 size-1.5 rounded-full bg-status-attention" /><span id="thread-attention-summary" class="sr-only">{attentionCount()} {attentionCount() === 1 ? "item needs" : "items need"} your attention</span></Show></button>
      <Show when={threadState.focusedId !== null && !globalArtifacts()}><svg class="pointer-events-none absolute top-[58px] left-12 hidden h-8 w-4 text-border split:block" viewBox="0 0 16 32" fill="none" aria-hidden="true" data-slot="thread-connector"><path d="M0 24h4c8 0 12-4 12-12V0" stroke="currentColor" /></svg></Show>
      <button class={barButton} aria-label="New Space or Task" title="New Space or Task" onClick={() => openThreadCreate(null)}><Plus class="size-5" /><span class="split:hidden">New</span></button>
      <button class={barButton} aria-label="All artifacts" title="All artifacts" aria-pressed={globalArtifacts() ? "true" : "false"} onClick={() => setGlobalArtifacts(value => !value)}><LayoutGrid class="size-5" /><span class="split:hidden">Artifacts</span></button>
      <button class={barButton} aria-label="Processes" title="Processes" onClick={openProcesses}><Activity class="size-5" /><span class="split:hidden">Processes</span></button>
      <div class="hidden split:block split:flex-1" />
      <button class={`${iconButton} hidden split:inline-flex`} aria-label="Settings" title="Settings" onClick={() => openSettings()}><Settings class="size-5" /></button>
    </nav>
    <ThreadIconPicker />
    <ThreadNavigation mode={navigationMode()} intent={threadNavigationIntent()} onClose={closeNavigation} onSelect={selectThread} onExpand={() => openNavigation()} />
    <ThreadCreate onSelect={selectThread} />
    <div class="flex min-h-0 min-w-0 flex-1 flex-col">
      <Show when={state.connection !== "connected"}><div class="flex shrink-0 justify-end px-3 pt-2"><ConnectionPill /></div></Show>
      <div class="flex min-h-0 flex-1 gap-2 py-2 pr-2 pl-2 sm:gap-3 sm:pr-3">
        <Show when={threadState.ready && historyId() && threadState.focusedId !== null && threadState.threads.some(thread => thread.id === threadState.focusedId) ? { id: threadState.focusedId!, history: historyId()! } : null} keyed fallback={<ThreadStart globalArtifacts={globalArtifacts()} browsable={navigationMode() !== "docked"} onSelect={selectThread} />} >{focused => <ThreadConversation id={focused.id} historyId={focused.history} attachments={attachmentsFor(focused.id)} globalArtifacts={globalArtifacts()} onConversation={() => setGlobalArtifacts(false)} />}</Show>
        <Show when={!sideCollapsed()} fallback={<Show when={collapsedShowcase()}>
          <button type="button" data-slot="collapsed-pane-tab" class="flex w-8 shrink-0 items-center justify-center rounded-lg border border-border text-meta text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" aria-label="Show the showcase and close the utility pane" title="Show the showcase" onClick={closeRightRegion}>
            <span class="[writing-mode:vertical-rl] rotate-180">Showcase</span>
          </button>
        </Show>}>
          <ArtifactSurface />
          <ShowcaseSurface />
        </Show>
        <CanvasRail /><CanvasSheet /><ProcessesSheet /><SettingsSheet />
      </div>
    </div>
  </div>;
}
