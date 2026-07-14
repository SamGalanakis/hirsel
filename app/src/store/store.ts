import { untrack } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { reduce } from "./reducer";
import { type Action, type AppState, initialState } from "./types";

export interface ComposerDraft {
  /** The Anchor message id to quote in the composer ("Reply" pre-quotes the
   * anchor - the user still types their own body). */
  ref: number;
}

/** The one exclusive owner of the desktop right region / phone secondary sheet.
 * Exactly one of these is showing at a time (the single-slot model): `pings` is
 * the idle resting state (the standing Pings rail on desktop, the Tray shelf on
 * phone), and everything else takes the slot until dismissed — closing any of
 * them returns to `pings`. This replaces the four independent flags
 * (processesOpen/settingsOpen/canvasOpen + `activeSideChatSc !== null`) that
 * used to fight over the region: with one enum, inactive panes UNMOUNT (no
 * hidden-focus leak, nothing to clip) and "last explicit user action wins." */
export type RightRegion = "pings" | "sideChat" | "canvas" | "processes" | "settings";

/** Cross-view navigation/UI state layered on top of the protocol AppState.
 * This is still "one store": the protocol side is kept in a pure,
 * independently-tested reducer (see reducer.ts) while this thin UI slice
 * (sheet/overlay visibility, one-shot scroll/prefill requests) is plain
 * setters, since it has no wire-protocol semantics to unit test.
 *
 * v1.6 (Tray): Chat is the whole app now, so there is no more tab-switching
 * plumbing. Inbox lives in the Tray (an overlay local to ChatView, expanded
 * state kept here so `goToChat` can centrally collapse it — e.g. "View in
 * chat" from inside the expanded tray).
 *
 * v2.3 (single-owner right region): the right region is owned by one exclusive
 * `rightRegion` enum rather than a set of independent booleans. */
/** The root surface of the app (ADR-0012 cutover). `queue` is the home — the
 * vertical event scroller; `chat` is the drill-in shell (the 3-pane
 * chat/side-chat/inspectors layout) reached from a judgment's Discuss, the
 * NavRail, or the phone overflow. The scroller is the phone home and the desktop
 * center measure; every summoned pane (Processes/Settings/Canvas/Side Chat)
 * lives in the chat shell, so opening one drills in. */
export type Home = "queue" | "chat";

interface UiState {
  /** The root surface: the queue scroller (home) or the chat drill-in shell.
   * Default `queue` (ADR-0012). */
  home: Home;
  /** Tray overlay expanded/collapsed. Only ever set true by an explicit tap on
   * the shelf (or the equivalent test action) — never auto-expanded. Orthogonal
   * to `rightRegion`: it is the phone Pings overlay's expand state within the
   * `pings` region (the desktop rail ignores it). */
  trayExpanded: boolean;
  /** The single owner of the right region (desktop) / secondary full-screen
   * sheet (phone). Default `pings`. Every pane switch sets exactly this — so
   * nothing lingers behind — and closing any pane returns it to `pings`. */
  rightRegion: RightRegion;
  /** Set when something (a quoted ref, a quick reply) wants Chat to scroll to
   * and highlight a message; consumed once then cleared. */
  scrollToMessageId: number | null;
  /** Set when "Reply" pre-quotes an Inbox Item's anchor into the composer. */
  composerDraft: ComposerDraft | null;
  /** Set when something wants Chat's composer pre-filled with body text (v1.4
   * "Ask to stop"); consumed once by the Composer then cleared. */
  composerPrefill: string | null;
  /** v2.0 (ADR-0008): the `sc` of the current Side Chat — the DATA of which one
   * the `sideChat` region shows. The pane SELECTION is `rightRegion` (v2.3): the
   * sheet is on screen only while `rightRegion === "sideChat"`, but the sc stays
   * set when you leave its pane so the side chat is alive/resumable underneath.
   * One at a time (critique, binding); every other in-progress side chat is "in
   * progress · resume" from its Ping card. Deliberately never set by a
   * cold-reconnect reconciliation — resuming is always a deliberate tap. */
  activeSideChatSc: string | null;
  /** v2.0: the Ping id whose Discuss/Resume tap is awaiting a `sideChatRefs`
   * entry before the sheet can open (its `sc` isn't known yet on a fresh
   * Discuss). Consumed once ChatView's effect sees the matching ref appear,
   * which then sets `activeSideChatSc`. Resume already has a live ref, so it
   * resolves on the very next tick. */
  pendingSideChatPingId: number | null;
  /** A post-auth protocol `error` frame's reason, surfaced as a visible inline
   * banner in Chat (rather than only `console.error`). Set by the ws client,
   * dismissed by the Owner. Lives in the UI slice — not the wire-protocol
   * AppState — since it is presentational, not sync state. */
  protocolError: string | null;
  /** One-shot: which Settings section to scroll into view when the Settings
   * pane opens (e.g. the phone overflow "Model settings" row opens Settings
   * scrolled to Models, an honest affordance instead of landing on Appearance).
   * Consumed once by SettingsPanel then cleared. `null` = open at the top. */
  settingsScrollTarget: SettingsSection | null;
  /** One-shot: a drill-in that should land with the main composer focused (the
   * phone queue-home chat bar's dormant pill — tap it to open Chat with the real
   * composer focused and the keyboard up). Set by `goToChat({ focusComposer })`,
   * consumed once by ChatView on mount then cleared. */
  composerAutofocus: boolean;
}

/** A jump-to target within the Settings pane, used by callers that open
 * Settings pointed at a specific section (see `settingsScrollTarget`). */
export type SettingsSection = "models";

type Store = AppState & UiState;

function initialStore(): Store {
  return {
    ...initialState(),
    home: "queue",
    trayExpanded: false,
    rightRegion: "pings",
    scrollToMessageId: null,
    composerDraft: null,
    composerPrefill: null,
    activeSideChatSc: null,
    pendingSideChatPingId: null,
    protocolError: null,
    settingsScrollTarget: null,
    composerAutofocus: false,
  };
}

const [state, setState] = createStore<Store>(initialStore());

/** Snapshot the protocol-facing slice to hand to the pure reducer. */
function appSnapshot(): AppState {
  return {
    messages: state.messages,
    pings: state.pings,
    events: state.events,
    eventDecideOverrides: state.eventDecideOverrides,
    eventsSnapshotSeq: state.eventsSnapshotSeq,
    agentActivity: state.agentActivity,
    connection: state.connection,
    lastSeenMsgId: state.lastSeenMsgId,
    hostVersion: state.hostVersion,
    model: state.model,
    subagentModels: state.subagentModels,
    pendingSends: state.pendingSends,
    uploads: state.uploads,
    processes: state.processes,
    turnEvents: state.turnEvents,
    turnDetails: state.turnDetails,
    removedIds: state.removedIds,
    unreadOverrides: state.unreadOverrides,
    resolveOverrides: state.resolveOverrides,
    sideChatRefs: state.sideChatRefs,
    sideChats: state.sideChats,
    pendingSideSends: state.pendingSideSends,
    awaitingConclusions: state.awaitingConclusions,
    conclusionChips: state.conclusionChips,
    lastConclusion: state.lastConclusion,
    views: state.views,
  };
}

/** Apply the same pure `reduce` used by the tests, then push the result into
 * the fine-grained store. The two rendered arrays (messages, inbox) go through
 * `reconcile` keyed by `id` so only the DOM bound to genuinely-changed rows
 * re-renders (messages + pings); the small scalar/never-rendered fields are set directly.
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
    setState("pings", reconcile(next.pings, { key: "id" }));
    // Typed event queue: reconcile keyed by id so only the DOM bound to a
    // genuinely-changed event (a re-upsert / decide flip) re-renders.
    setState("events", reconcile(next.events, { key: "id" }));
    setState("eventDecideOverrides", next.eventDecideOverrides);
    setState("eventsSnapshotSeq", next.eventsSnapshotSeq);
    setState("pendingSends", next.pendingSends);
    setState("removedIds", next.removedIds);
    setState("unreadOverrides", next.unreadOverrides);
    setState("resolveOverrides", next.resolveOverrides);
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
    // Nullable object slices: a plain set (like hostVersion/agentActivity), not
    // `reconcile`, since reconcile is for arrays/keyed objects and these swap to
    // a whole new snapshot/catalog (or null) on each change.
    setState("model", next.model);
    setState("subagentModels", next.subagentModels);
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
    // Generative-UI tier: reconcile keyed by instance_id so only the DOM bound
    // to a genuinely-changed view (a re-upsert / update-in-place) re-renders.
    setState("views", reconcile(next.views, { key: "instance_id" }));
  });
}

/** Land on Chat: returns the right region to its idle `pings` resting state
 * (so any Processes/Settings/Canvas/Side-Chat pane covering the surface being
 * navigated to unmounts), collapses the Tray overlay, then applies any one-shot
 * scroll/draft/prefill request. The current side chat's DATA (`activeSideChatSc`)
 * is left alive/resumable — only the pane selection changes. */
export function goToChat(opts?: {
  scrollToMessageId?: number;
  composerDraft?: ComposerDraft;
  /** Pre-fill the composer with this body (v1.4 "Ask to stop"). */
  composerPrefill?: string;
  /** Land with the main composer focused + keyboard up (the phone chat bar's
   * dormant pill). One-shot, consumed by ChatView on mount. */
  focusComposer?: boolean;
}): void {
  setState({
    home: "chat",
    rightRegion: "pings",
    trayExpanded: false,
    scrollToMessageId: opts?.scrollToMessageId ?? null,
    composerDraft: opts?.composerDraft ?? null,
    composerPrefill: opts?.composerPrefill ?? null,
    composerAutofocus: opts?.focusComposer ?? false,
  });
}

/** Return to the queue scroller home (the root). Leaves the chat shell state
 * (rightRegion, active side chat) alive underneath so a later drill-in resumes
 * where it was. */
export function goToQueue(): void {
  setState("home", "queue");
}

/** Drill into the chat shell from the queue (a judgment's Discuss, or any nav
 * that needs the shell). Does not change the right region — the caller sets the
 * pane it wants (or leaves the resting Pings rail). */
export function goToChatDrillIn(): void {
  setState("home", "chat");
}

/** The single low-level setter for the exclusive right region. Every intent
 * helper below routes through this so "one pane owns the region" holds by
 * construction — setting a region is all it takes to unmount whatever held it. */
export function setRightRegion(region: RightRegion): void {
  setState("rightRegion", region);
}

/** Close whatever pane owns the right region, back to the `pings` resting state. */
export function closeRightRegion(): void {
  setState("rightRegion", "pings");
}

/** Select the Pings resting state (the standing rail on desktop / the Tray on
 * phone). Drills into the chat shell if we were on the queue home, since every
 * right-region pane lives there. */
export function openPings(): void {
  setState({ home: "chat", rightRegion: "pings" });
}

/** Dock the Processes inspector into the right region (chat shell). */
export function openProcesses(): void {
  setState({ home: "chat", rightRegion: "processes" });
}

/** Dock the Settings inspector into the right region. An optional `section`
 * points the pane at a specific group on open (e.g. the phone overflow "Model
 * settings" row opens Settings scrolled to Models); omitted opens at the top. */
export function openSettings(section?: SettingsSection): void {
  setState({ home: "chat", rightRegion: "settings", settingsScrollTarget: section ?? null });
}

/** Consume the one-shot Settings scroll target (SettingsPanel reads it on
 * mount, scrolls, then clears it so a later open lands at the top). */
export function clearSettingsScrollTarget(): void {
  setState("settingsScrollTarget", null);
}

/** Surface the Canvas view in the right region. Collapses the phone Tray
 * overlay so the region isn't double-booked there. */
export function showCanvas(): void {
  setState({ home: "chat", rightRegion: "canvas", trayExpanded: false });
}

/** Open the Side Chat pane: records which sc backs it AND selects the region.
 * Setting the region here is the ONLY thing that ever shows the sheet — cold
 * reconnect reconciliation seeds `sideChatRefs`/`sideChats` but never touches
 * this, so a live side chat never auto-reopens on launch (critique, binding). */
export function openSideChat(sc: string): void {
  setState({ home: "chat", activeSideChatSc: sc, rightRegion: "sideChat" });
}

/** Set only the current-side-chat DATA (which sc), without changing the pane
 * selection. Used by teardown paths that clear the data after a discard/
 * conclusion. Passing a non-null sc without selecting the region is intentional
 * only in tests; production opens via `openSideChat`. */
export function setActiveSideChatSc(sc: string | null): void {
  setState("activeSideChatSc", sc);
}

/** Discuss/Resume tapped on a Ping card: fires `open_side_chat` (the caller
 * does that) and records which Ping's sheet to open the moment its ref appears
 * (see `pendingSideChatPingId`). */
export function requestSideChatOpen(pingId: number): void {
  setState("pendingSideChatPingId", pingId);
}

export function clearPendingSideChatOpen(): void {
  setState("pendingSideChatPingId", null);
}

export function clearLastConclusion(): void {
  dispatch({ type: "clear_last_conclusion" });
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

/** Quote a specific message into the main composer ("Reply" from a message's
 * actions menu). Self-contained UI-slice write — deliberately NOT routed
 * through the reducer/hello_ok path — it just seeds the same `composerDraft`
 * the Ping "Reply" flow uses, which ChatView resolves to the reply target and
 * the Composer renders as "Replying to…". */
export function setComposerReplyTarget(ref: number): void {
  setState("composerDraft", { ref });
}

/** Surface a post-auth protocol `error` as a visible inline banner. */
export function setProtocolError(detail: string): void {
  setState("protocolError", detail);
}

export function clearProtocolError(): void {
  setState("protocolError", null);
}

export function clearComposerPrefill(): void {
  setState("composerPrefill", null);
}

export function clearComposerAutofocus(): void {
  setState("composerAutofocus", false);
}

/** The reactive store proxy: components read `state.messages`, `state.connection`,
 * etc. directly and Solid tracks the exact reads. Also read by the WebSocket
 * client module (which is not a component) for the `pendingSends` replay. */
export { state };
