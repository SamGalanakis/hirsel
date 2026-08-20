import { untrack } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import type { EventItem } from "../protocol";
import { reduce } from "./reducer";
import { projectEvents } from "./selectors";
import { type Action, type AppState, initialState } from "./types";

/** Exactly one temporary utility can cover the task world at a time. */
export type RightRegion = "none" | "canvas" | "processes" | "settings";

/** Small local UI state layered over the protocol reducer's authoritative data. */
interface UiState {
  rightRegion: RightRegion;
  composerPrefill: string | null;
  protocolError: string | null;
  settingsScrollTarget: SettingsSection | null;
  /** The one focused Task, or `null` for the ambient field. Ambient is the
   * absence of focus, so `null` is the resting value, not a mode. It lives here
   * rather than inside the shell because focus is now reachable from outside the
   * task tree — the Esc ladder and the ⌘K "Clear task focus" command both need
   * to leave a Task without owning the shell's internals. */
  focusedTaskId: number | null;
}

/** A jump-to target within the Settings pane, used by callers that open
 * Settings pointed at a specific section (see `settingsScrollTarget`). */
export type SettingsSection = "models";

type Store = AppState & UiState;

function initialStore(): Store {
  return {
    ...initialState(),
    rightRegion: "none",
    composerPrefill: null,
    protocolError: null,
    settingsScrollTarget: null,
    focusedTaskId: null,
  };
}

const [state, setState] = createStore<Store>(initialStore());

/** The projection of `state.events` through `state.eventOverrides` (R6-1),
 * held in its own store rather than derived in a memo for ONE reason: row
 * identity. Every task surface renders through `<For>`, which is keyed by object
 * reference, so a memo minting fresh objects on each override change would tear
 * down and rebuild the row's DOM (losing focus mid-gesture). Reconciled by `id`,
 * the row object survives an optimistic flip and only the changed field notifies.
 * Written synchronously inside `dispatch`, so a caller reading it right after a
 * dispatch (or right before one) always sees the settled truth. */
const [projection, setProjection] = createStore<{ events: EventItem[] }>({ events: [] });

/** Snapshot the protocol-facing slice to hand to the pure reducer. */
function appSnapshot(): AppState {
  return {
    messages: state.messages,
    hasEarlierMessages: state.hasEarlierMessages,
    hasLaterMessages: state.hasLaterMessages,
    events: state.events,
    eventOverrides: state.eventOverrides,
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
    lastTurnEvents: state.lastTurnEvents,
    turnDetails: state.turnDetails,
    removedIds: state.removedIds,
    views: state.views,
  };
}

/** Apply the same pure `reduce` used by the tests, then push the result into
 * the fine-grained store. The rendered arrays go through
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
    setState("hasEarlierMessages", next.hasEarlierMessages);
    setState("hasLaterMessages", next.hasLaterMessages);
    // Task collection: reconcile typed wire Events by id so only the DOM bound to a
    // genuinely-changed event (a re-upsert / decide flip) re-renders.
    setState("events", reconcile(next.events, { key: "id" }));
    // The one optimistic layer over those events. `reconcile` (rather than a
    // plain set) keeps the per-id entries stable, so a gesture on one event
    // never invalidates the projection of every other.
    setState("eventOverrides", reconcile(next.eventOverrides));
    setProjection(
      "events",
      reconcile(projectEvents(next.events, next.eventOverrides), { key: "id" }),
    );
    setState("pendingSends", next.pendingSends);
    setState("removedIds", next.removedIds);
    setState("uploads", reconcile(next.uploads, { key: "clientId" }));
    setState("processes", reconcile(next.processes, { key: "id" }));
    setState("turnEvents", next.turnEvents);
    // Never rendered directly (the parking slot for a turn whose idle boundary
    // beat its committing message) — a plain set, no reconcile needed.
    setState("lastTurnEvents", next.lastTurnEvents);
    // A plain set of this Record-of-arrays can retain live proxy references;
    // reconcile applies the structural diff safely. See retainTurnDetails.
    setState("turnDetails", reconcile(next.turnDetails));
    setState("agentActivity", next.agentActivity);
    setState("connection", next.connection);
    setState("lastSeenMsgId", next.lastSeenMsgId);
    setState("hostVersion", next.hostVersion);
    // Nullable object slices: a plain set (like hostVersion/agentActivity), not
    // `reconcile`, since reconcile is for arrays/keyed objects and these swap to
    // a whole new snapshot/catalog (or null) on each change.
    setState("model", next.model);
    setState("subagentModels", next.subagentModels);
    // Generative-UI tier: reconcile keyed by instance_id so only the DOM bound
    // to a genuinely-changed view (a re-upsert / update-in-place) re-renders.
    setState("views", reconcile(next.views, { key: "instance_id" }));
  });
}

/** THE Event list every surface reads: the wire truth in `state.events` with
 * the optimistic `eventOverrides` layer folded in (R6-1). Selectors, components
 * and the action helpers all take these plain projected events, so no caller
 * ever threads override ids around. Reactive like any store read — components
 * track exactly the rows and fields they touch. */
export function effectiveEvents(): EventItem[] {
  return projection.events;
}

// ---- Task focus -------------------------------------------------------------

/** Focus a Task, or return to ambient when it is already the focused one —
 * selecting the focused task again clears focus (DESIGN §4). */
export function toggleTaskFocus(id: number): void {
  setState("focusedTaskId", (current) => (current === id ? null : id));
}

/** Focus a Task unconditionally. Distinct from `toggleTaskFocus` for callers
 * that are not a selection gesture — the load-time auto-focus in particular,
 * where "toggle" would be the wrong contract. */
export function focusTask(id: number): void {
  setState("focusedTaskId", id);
}

/** Leave the focused Task for the ambient field. The exit path behind Esc, the
 * focused chip's × affordance, and the ⌘K "Clear task focus" command. */
export function clearTaskFocus(): void {
  setState("focusedTaskId", null);
}

/** Drop focus when the focused Task is no longer in the visible field (archived,
 * swept, resynced away). Untracked like `dispatch`: it is called from the shell's
 * reconciliation effect and must not subscribe that effect to its own write. */
export function reconcileTaskFocus(visibleIds: number[]): void {
  untrack(() => {
    const focused = state.focusedTaskId;
    if (focused !== null && !visibleIds.includes(focused)) setState("focusedTaskId", null);
  });
}

/** The single low-level setter for the exclusive right region. Every intent
 * helper below routes through this so "one pane owns the region" holds by
 * construction — setting a region is all it takes to unmount whatever held it. */
export function setRightRegion(region: RightRegion): void {
  setState("rightRegion", region);
}

/** Close whatever pane owns the right region, back to the `none` resting state. */
export function closeRightRegion(): void {
  setState("rightRegion", "none");
}

/** Dock the Processes inspector into the task world's utility region. */
export function openProcesses(): void {
  setState("rightRegion", "processes");
}

/** Dock the Settings inspector into the right region. An optional `section`
 * points the pane at a specific group on open (e.g. the phone overflow "Model
 * settings" row opens Settings scrolled to Models); omitted opens at the top. */
export function openSettings(section?: SettingsSection): void {
  setState({ rightRegion: "settings", settingsScrollTarget: section ?? null });
}

/** Consume the one-shot Settings scroll target (SettingsPanel reads it on
 * mount, scrolls, then clears it so a later open lands at the top). */
export function clearSettingsScrollTarget(): void {
  setState("settingsScrollTarget", null);
}

/** Surface the Canvas view in the right region. */
export function showCanvas(): void {
  setState("rightRegion", "canvas");
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

/** Seed the always-mounted Hirsel composer without changing its task subject. */
export function prefillComposer(text: string): void {
  setState("composerPrefill", text);
}

/** The reactive store proxy: components read `state.messages`, `state.connection`,
 * etc. directly and Solid tracks the exact reads. Also read by the WebSocket
 * client module (which is not a component) for the `pendingSends` replay. */
export { state };
