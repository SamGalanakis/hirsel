import { untrack, createStore, reconcile } from "solid-js";
import { reduce } from "./reducer";
import { type Action, type AppState, initialState } from "./types";
export type RightRegion = "none" | "canvas" | "processes" | "settings";
export type SettingsTab = "appearance" | "agents" | "providers" | "connection" | "notifications" | "guide" | "about" | "plugins";
interface UiState { rightRegion: RightRegion; protocolError: string | null; settingsTab: SettingsTab | null; promptsRevision: number; providersRevision: number }
const [state, setState] = createStore<AppState & UiState>({ ...initialState(), rightRegion: "none", protocolError: null, settingsTab: null, promptsRevision: 0, providersRevision: 0 });
/** The keyed collections: reconciled by identity so rows keep their nodes.
 * Every other AppState field is copied by the loop below, so a field added to
 * `types.ts` renders without anyone remembering to name it here. */
const RECONCILED = { processes: "id", views: "instance_id" } as const satisfies Partial<Record<keyof AppState, string>>;
/** Every AppState field, taken from the one place a new field must be declared. */
const APP_STATE_KEYS = Object.keys(initialState()) as (keyof AppState)[];
export function dispatch(action: Action): void {
  untrack(() => setState(draft => {
    const next = reduce(state, action);
    reconcile(next.processes, RECONCILED.processes)(draft.processes);
    reconcile(next.views, RECONCILED.views)(draft.views);
    for (const key of APP_STATE_KEYS) {
      if (key in RECONCILED) continue;
      Object.assign(draft, { [key]: next[key] });
    }
    if (action.type === "prompts_changed") draft.promptsRevision++;
    if (action.type === "providers_changed") draft.providersRevision++;
  }));
}
export function closeRightRegion(): void { setState(draft => { draft.rightRegion = "none"; }); }
export function openProcesses(): void {
  setState(draft => { draft["rightRegion"] = "processes"; });
}

/** Summon Settings, optionally on a named tab (`openSettings("providers")`).
 * Omitted opens on Appearance, the first tab. */
export function openSettings(tab?: SettingsTab): void {
  setState(draft => { Object.assign(draft, { rightRegion: "settings", settingsTab: tab ?? null }); });
}

/** Consume the one-shot Settings tab target: the panel reads it on mount to
 * choose its landing tab, then clears it so a later open starts at the top of
 * the rail again. */
export function clearSettingsTab(): void {
  setState(draft => { draft["settingsTab"] = null; });
}

/** Surface the Canvas view in the right region. */
export function showCanvas(): void {
  setState(draft => { draft["rightRegion"] = "canvas"; });
}

/** Surface a post-auth protocol `error` as a visible inline banner. */
export function setProtocolError(detail: string): void {
  setState(draft => { draft["protocolError"] = detail; });
}

export function clearProtocolError(): void {
  setState(draft => { draft["protocolError"] = null; });
}

/** The reactive store proxy: components read `state.processes`, `state.connection`,
 * etc. directly and Solid tracks the exact reads. Also read by the WebSocket
 * client module (which is not a component) for operational state. */
export { state };
