import { untrack, createStore, reconcile } from "solid-js";
import { reduce } from "./reducer";
import { type Action, type AppState, initialState } from "./types";
export type RightRegion = "none" | "canvas" | "processes" | "settings";
export type SettingsTab = "appearance" | "agents" | "providers" | "connection" | "notifications" | "guide" | "about" | "plugins";
interface UiState { rightRegion: RightRegion; composerPrefill: string | null; protocolError: string | null; settingsTab: SettingsTab | null; promptsRevision: number; providersRevision: number }
const [state, setState] = createStore<AppState & UiState>({ ...initialState(), rightRegion: "none", composerPrefill: null, protocolError: null, settingsTab: null, promptsRevision: 0, providersRevision: 0 });
export function dispatch(action: Action): void {
  untrack(() => setState(draft => {
    const next = reduce(state, action);
    reconcile(next.processes, "id")(draft.processes);
    reconcile(next.views, "instance_id")(draft.views);
    draft.connection = next.connection; draft.hostVersion = next.hostVersion;
    draft.model = next.model; draft.subagentModels = next.subagentModels;
    draft.prompts = next.prompts; draft.providers = next.providers;
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

export function clearComposerPrefill(): void {
  setState(draft => { draft["composerPrefill"] = null; });
}

/** Seed the always-mounted Hirsel composer without changing its Thread subject. */
export function prefillComposer(text: string): void {
  setState(draft => { draft["composerPrefill"] = text; });
}

/** The reactive store proxy: components read `state.processes`, `state.connection`,
 * etc. directly and Solid tracks the exact reads. Also read by the WebSocket
 * client module (which is not a component) for operational state. */
export { state };
