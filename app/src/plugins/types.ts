// The plugin tier's shared vocabulary: the wire records the Host serves and the
// `api` object an in-repo UI module is handed. Kept free of imports from the
// rest of the app so a plugin author can read this one file and know the whole
// contract.
import type { JSX } from "solid-js";

/** The mount points a plugin may render into. A slot is a promise about
 * placement AND about the `ctx` its components receive — adding one means
 * adding a `<PluginSlot>` in the view that owns that region. */
export type SlotName = "settings.section" | "task.panel" | "home.section";

export const SLOT_NAMES: readonly SlotName[] = [
  "settings.section",
  "task.panel",
  "home.section",
];

/** What a slot component is told about where it is rendering. Deliberately
 * minimal: `settings.section` and `home.section` pass `{}`; `task.panel` passes
 * the focused Task's id. Fields are optional so one component type covers every
 * slot; a component reads only the field its slot documents. */
export interface SlotCtx {
  taskId?: number;
}

/** A plugin-supplied Solid component. It runs once (Solid semantics) inside an
 * error boundary owned by the host view. */
export type SlotComponent = (props: { ctx: SlotCtx }) => JSX.Element;

/** A settings field a plugin declares. `secret` values are never returned in
 * clear: the Host reports the literal string `"<set>"` when one is stored and
 * `null` when it is not. */
export type PluginSettingKind = "string" | "boolean" | "secret";

export interface PluginSettingSpec {
  key: string;
  label: string;
  kind: PluginSettingKind;
  default?: string | boolean | null;
}

export type PluginSettingValue = string | boolean | null;

/** The sentinel a `secret` value carries when the Host holds one. */
export const SECRET_SET = "<set>";

/** `running` — the plugin's daemon is up. `disabled` — switched off by the
 * Owner. `errored` — the daemon crash-looped; `error` carries the reason. */
export type PluginState = "running" | "disabled" | "errored";

/** One entry of `GET /api/plugins` — every installed plugin. */
export interface PluginInfo {
  id: string;
  label: string;
  version: string;
  state: PluginState;
  error?: string | null;
  settings: PluginSettingSpec[];
  values: Record<string, PluginSettingValue>;
}

export interface PluginListResponse {
  plugins: PluginInfo[];
}

/** The object a UI module's default export is called with — the whole of what a
 * plugin may do to the app. Everything is scoped to the calling plugin: `fetch`
 * can only reach its own Host router, `onPush` only sees its own frames, and a
 * registered component is always attributed to it. */
export interface PluginApi {
  /** This plugin's stable id — its folder name under the repo's `plugins/`. */
  readonly id: string;
  /** Its human label, as the Host reports it. */
  readonly label: string;
  slots: {
    /** Mount `component` into `slot`. Returns an unregister function; the
     * loader also unregisters everything automatically when the plugin is torn
     * down, so most modules ignore the return value. */
    register: (slot: SlotName, component: SlotComponent) => () => void;
  };
  /** Authenticated `fetch` against this plugin's own Host router. `path` is
   * relative to that router's root (`/greet` → `/api/plugins/<id>/greet`).
   * Platform `fetch` semantics: a non-2xx resolves, it does not throw. */
  fetch: (path: string, init?: RequestInit) => Promise<Response>;
  /** Subscribe to `plugin_push` frames for this plugin and `topic`. Returns an
   * unsubscribe function. */
  onPush: (topic: string, handler: (data: unknown) => void) => () => void;
}

/** A UI module's default export: called once with the api, optionally returning
 * a disposer that runs when the plugin is torn down. */
export type PluginFactory = (api: PluginApi) => void | (() => void);
