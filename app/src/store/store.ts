import { untrack } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { reduce } from "./reducer";
import { type Action, type AppState, initialState } from "./types";

export interface ComposerDraft {
  /** The Anchor message id to quote in the composer ("Reply" pre-quotes the
   * anchor - the user still types their own body). */
  ref: number;
}

/** Cross-view navigation/UI state layered on top of the protocol AppState.
 * This is still "one store": the protocol side is kept in a pure,
 * independently-tested reducer (see reducer.ts) while this thin UI slice
 * (sheet/overlay visibility, one-shot scroll/prefill requests) is plain
 * setters, since it has no wire-protocol semantics to unit test.
 *
 * v1.6 (Tray): Chat is the whole app now, so there is no more tab-switching
 * plumbing. Inbox lives in the Tray (an overlay local to ChatView, expanded
 * state kept here so `goToChat` can centrally collapse it — e.g. "View in
 * chat" from inside the expanded tray). Processes is a header-launched
 * full-screen sheet, tracked by `processesOpen`. */
interface UiState {
  /** Tray overlay expanded/collapsed. Only ever set true by an explicit tap on
   * the shelf (or the equivalent test action) — never auto-expanded. */
  trayExpanded: boolean;
  /** Processes full-screen sheet open/closed, launched from the header icon. */
  processesOpen: boolean;
  /** Set when something (a quoted ref, a quick reply) wants Chat to scroll to
   * and highlight a message; consumed once then cleared. */
  scrollToMessageId: number | null;
  /** Set when "Reply" pre-quotes an Inbox Item's anchor into the composer. */
  composerDraft: ComposerDraft | null;
  /** Set when something wants Chat's composer pre-filled with body text (v1.4
   * "Ask to stop"); consumed once by the Composer then cleared. */
  composerPrefill: string | null;
  /** v2.0 (ADR-0008): the `sc` of the one Side Chat sheet currently on screen,
   * or null. One sheet at a time (critique, binding); every other in-progress
   * side chat is "in progress · resume" from its Inbox card. Deliberately
   * never set by a cold-reconnect reconciliation — resuming is always a
   * deliberate tap (critique: "do not auto-reopen"). */
  activeSideChatSc: string | null;
  /** v2.0: the Inbox item id whose Discuss/Resume tap is awaiting a
   * `sideChatRefs` entry before the sheet can open (its `sc` isn't known yet
   * on a fresh Discuss). Consumed once ChatView's effect sees the matching
   * ref appear, which then sets `activeSideChatSc`. Resume already has a
   * live ref, so it resolves on the very next tick. */
  pendingSideChatItemId: number | null;
}

type Store = AppState & UiState;

function initialStore(): Store {
  return {
    ...initialState(),
    trayExpanded: false,
    processesOpen: false,
    scrollToMessageId: null,
    composerDraft: null,
    composerPrefill: null,
    activeSideChatSc: null,
    pendingSideChatItemId: null,
  };
}

const [state, setState] = createStore<Store>(initialStore());

/** Snapshot the protocol-facing slice to hand to the pure reducer. */
function appSnapshot(): AppState {
  return {
    messages: state.messages,
    inbox: state.inbox,
    agentActivity: state.agentActivity,
    connection: state.connection,
    lastSeenMsgId: state.lastSeenMsgId,
    pendingSends: state.pendingSends,
    uploads: state.uploads,
    processes: state.processes,
    turnEvents: state.turnEvents,
    turnDetails: state.turnDetails,
    removedIds: state.removedIds,
    unreadOverrides: state.unreadOverrides,
    sideChatRefs: state.sideChatRefs,
    sideChats: state.sideChats,
    pendingSideSends: state.pendingSideSends,
    awaitingConclusions: state.awaitingConclusions,
    conclusionChips: state.conclusionChips,
    lastConclusion: state.lastConclusion,
  };
}

/** Apply the same pure `reduce` used by the tests, then push the result into
 * the fine-grained store. The two rendered arrays (messages, inbox) go through
 * `reconcile` keyed by `id` so only the DOM bound to genuinely-changed rows
 * re-renders; the small scalar/never-rendered fields are set directly.
 *
 * The whole body is `untrack`ed: dispatch is an imperative command, and it is
 * sometimes invoked synchronously from inside a reactive scope (e.g. the App
 * effect that opens the socket immediately dispatches connection_status). Its
 * internal `appSnapshot()` reads must NOT subscribe that caller to the store,
 * or the subsequent writes here would re-trigger it in an infinite loop. */
export function dispatch(action: Action): void {
  untrack(() => {
    const next = reduce(appSnapshot(), action);
    setState("messages", reconcile(next.messages, { key: "id" }));
    setState("inbox", reconcile(next.inbox, { key: "id" }));
    setState("pendingSends", next.pendingSends);
    setState("removedIds", next.removedIds);
    setState("unreadOverrides", next.unreadOverrides);
    setState("uploads", reconcile(next.uploads, { key: "clientId" }));
    setState("processes", reconcile(next.processes, { key: "id" }));
    setState("turnEvents", next.turnEvents);
    // `reconcile` (not a plain setState) is load-bearing here, same as
    // `sideChats` below: a plain `setState("turnDetails", plainObject)` has
    // the same same-shaped-Record-of-arrays failure mode that intermittently
    // landed a stale/empty nested array for sideChats (traced via a resume
    // losing its restored transcript) — reconcile's structural diff does not
    // have that hazard. See reducer.ts's `retainTurnDetails` for the matching
    // clone-through-every-entry half of this fix.
    setState("turnDetails", reconcile(next.turnDetails));
    setState("agentActivity", next.agentActivity);
    setState("connection", next.connection);
    setState("lastSeenMsgId", next.lastSeenMsgId);
    setState("sideChatRefs", next.sideChatRefs);
    // `reconcile` (not a plain setState) is load-bearing here too: a plain
    // `setState("sideChats", plainObject)` intermittently landed a stale/empty
    // nested array on a same-length array replacement (traced via a resume
    // losing its restored transcript) — reconcile's structural diff does not
    // have that failure mode.
    setState("sideChats", reconcile(next.sideChats));
    setState("pendingSideSends", next.pendingSideSends);
    setState("awaitingConclusions", next.awaitingConclusions);
    setState("conclusionChips", next.conclusionChips);
    setState("lastConclusion", next.lastConclusion);
  });
}

/** Land on Chat: closes the Processes sheet and collapses the Tray overlay
 * (both would otherwise cover the surface being navigated to), then applies
 * any one-shot scroll/draft/prefill request. */
export function goToChat(opts?: {
  scrollToMessageId?: number;
  composerDraft?: ComposerDraft;
  /** Pre-fill the composer with this body (v1.4 "Ask to stop"). */
  composerPrefill?: string;
}): void {
  setState({
    processesOpen: false,
    trayExpanded: false,
    activeSideChatSc: null,
    scrollToMessageId: opts?.scrollToMessageId ?? null,
    composerDraft: opts?.composerDraft ?? null,
    composerPrefill: opts?.composerPrefill ?? null,
  });
}

/** Open (or leave-alive-close) the one Side Chat sheet on screen. Setting a
 * `sc` here is the ONLY thing that ever shows the sheet — cold reconnect
 * reconciliation seeds `sideChatRefs`/`sideChats` but never touches this, so
 * a live side chat never auto-reopens on launch (critique, binding). */
export function setActiveSideChatSc(sc: string | null): void {
  setState("activeSideChatSc", sc);
}

/** Discuss/Resume tapped on an Inbox card: fires `open_side_chat` (the caller
 * does that) and records which item's sheet to open the moment its ref
 * appears (see `pendingSideChatItemId`). */
export function requestSideChatOpen(itemId: number): void {
  setState("pendingSideChatItemId", itemId);
}

export function clearPendingSideChatOpen(): void {
  setState("pendingSideChatItemId", null);
}

export function clearLastConclusion(): void {
  dispatch({ type: "clear_last_conclusion" });
}

export function setProcessesOpen(open: boolean): void {
  setState("processesOpen", open);
}

export function setTrayExpanded(open: boolean): void {
  setState("trayExpanded", open);
}

export function clearScrollTarget(): void {
  setState("scrollToMessageId", null);
}

export function clearComposerDraft(): void {
  setState("composerDraft", null);
}

export function clearComposerPrefill(): void {
  setState("composerPrefill", null);
}

/** The reactive store proxy: components read `state.messages`, `state.connection`,
 * etc. directly and Solid tracks the exact reads. Also read by the WebSocket
 * client module (which is not a component) for the `pendingSends` replay. */
export { state };
