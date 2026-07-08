import { create } from "zustand";
import { reduce } from "./reducer";
import { type AppState, initialState, type Action } from "./types";

export type Tab = "chat" | "inbox";

export interface ComposerDraft {
  /** The Anchor message id to quote in the composer ("Reply" pre-quotes the
   * anchor - the user still types their own body). */
  ref: number;
}

/** Cross-view navigation/UI state layered on top of the protocol AppState.
 * This is still "one store" (the spec's requirement) - the protocol side is
 * kept in a pure, independently-tested reducer (see reducer.ts) while this
 * thin UI slice (active tab, one-shot scroll/prefill requests) is plain
 * zustand setters, since it has no wire-protocol semantics to unit test. */
interface UiState {
  activeTab: Tab;
  /** Set when something (a quoted ref, a quick reply) wants Chat to scroll to
   * and highlight a message; consumed once then cleared. */
  scrollToMessageId: number | null;
  /** Set when "Reply" pre-quotes an Inbox Item's anchor into the composer. */
  composerDraft: ComposerDraft | null;
}

interface Store extends AppState, UiState {
  dispatch: (action: Action) => void;
  goToChat: (opts?: { scrollToMessageId?: number; composerDraft?: ComposerDraft }) => void;
  setActiveTab: (tab: Tab) => void;
  clearScrollTarget: () => void;
  clearComposerDraft: () => void;
}

export const useStore = create<Store>((set) => ({
  ...initialState(),
  activeTab: "chat",
  scrollToMessageId: null,
  composerDraft: null,

  dispatch: (action) => set((state) => reduce(state, action)),

  goToChat: (opts) =>
    set(() => ({
      activeTab: "chat",
      scrollToMessageId: opts?.scrollToMessageId ?? null,
      composerDraft: opts?.composerDraft ?? null,
    })),
  setActiveTab: (tab) => set({ activeTab: tab }),
  clearScrollTarget: () => set({ scrollToMessageId: null }),
  clearComposerDraft: () => set({ composerDraft: null }),
}));

/** Non-hook accessor for use inside the WebSocket client module, which is not
 * itself a React component. */
export const storeApi = useStore;
