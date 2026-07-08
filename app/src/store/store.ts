import { createStore, reconcile } from "solid-js/store";
import { reduce } from "./reducer";
import { type Action, type AppState, initialState } from "./types";

export type Tab = "chat" | "inbox";

export interface ComposerDraft {
  /** The Anchor message id to quote in the composer ("Reply" pre-quotes the
   * anchor - the user still types their own body). */
  ref: number;
}

/** Cross-view navigation/UI state layered on top of the protocol AppState.
 * This is still "one store": the protocol side is kept in a pure,
 * independently-tested reducer (see reducer.ts) while this thin UI slice
 * (active tab, one-shot scroll/prefill requests) is plain setters, since it
 * has no wire-protocol semantics to unit test. */
interface UiState {
  activeTab: Tab;
  /** Set when something (a quoted ref, a quick reply) wants Chat to scroll to
   * and highlight a message; consumed once then cleared. */
  scrollToMessageId: number | null;
  /** Set when "Reply" pre-quotes an Inbox Item's anchor into the composer. */
  composerDraft: ComposerDraft | null;
}

type Store = AppState & UiState;

function initialStore(): Store {
  return {
    ...initialState(),
    activeTab: "chat",
    scrollToMessageId: null,
    composerDraft: null,
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
  };
}

/** Apply the same pure `reduce` used by the tests, then push the result into
 * the fine-grained store. The two rendered arrays (messages, inbox) go through
 * `reconcile` keyed by `id` so only the DOM bound to genuinely-changed rows
 * re-renders; the small scalar/never-rendered fields are set directly. */
export function dispatch(action: Action): void {
  const next = reduce(appSnapshot(), action);
  setState("messages", reconcile(next.messages, { key: "id" }));
  setState("inbox", reconcile(next.inbox, { key: "id" }));
  setState("pendingSends", next.pendingSends);
  setState("agentActivity", next.agentActivity);
  setState("connection", next.connection);
  setState("lastSeenMsgId", next.lastSeenMsgId);
}

export function goToChat(opts?: {
  scrollToMessageId?: number;
  composerDraft?: ComposerDraft;
}): void {
  setState({
    activeTab: "chat",
    scrollToMessageId: opts?.scrollToMessageId ?? null,
    composerDraft: opts?.composerDraft ?? null,
  });
}

export function setActiveTab(tab: Tab): void {
  setState("activeTab", tab);
}

export function clearScrollTarget(): void {
  setState("scrollToMessageId", null);
}

export function clearComposerDraft(): void {
  setState("composerDraft", null);
}

/** The reactive store proxy: components read `state.messages`, `state.connection`,
 * etc. directly and Solid tracks the exact reads. Also read by the WebSocket
 * client module (which is not a component) for the `pendingSends` replay. */
export { state };
