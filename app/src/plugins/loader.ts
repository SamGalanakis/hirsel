// Boot-time plugin UI loading.
//
// Plugin UI lives IN THE REPO, at `<repo-root>/plugins/<id>/ui/index.tsx`, and
// is part of this app's build: Vite's glob import discovers every such module at
// build time and code-splits each into its own lazily-imported chunk. Installing
// a plugin means adding its folder and rebuilding — there is no served bundle,
// no runtime module resolution, and therefore no import map or Solid shim to
// keep straight. A plugin's UI shares the app's Solid instance because it IS
// part of the app.
//
// What is still decided at runtime is WHICH of those modules to initialise: a
// plugin folder present at build time must stay inert unless the Host reports
// its daemon enabled. So the glob is the candidate set and `GET /api/plugins` is
// the gate.
//
// The other constant is ISOLATION. A plugin is code the app hosts but does not
// own, so every step per plugin — the import, the default-export shape check,
// the factory call — is individually caught, logged, and recorded as a load
// failure, and the loop continues with the next plugin.
import { fetchPlugins, pluginFetch } from "./host";
import {
  recordLoadFailure,
  registerSlot,
  subscribePush,
  unregisterPlugin,
} from "./registry";
import type { PluginApi, PluginFactory, PluginInfo, SlotComponent, SlotName } from "./types";

/** Every in-repo UI module, lazily imported. Keys are the glob's own relative
 * paths (`../../../plugins/hello/ui/index.tsx`). `eager: false` keeps each in
 * its own chunk, so a plugin the Owner has switched off costs no bytes at
 * startup. */
const UI_MODULES = import.meta.glob("../../../plugins/*/ui/index.tsx");

/** `plugins/<id>/ui/index.tsx` → `<id>`. The folder name IS the plugin id, and
 * is what the Host's roster is matched against. */
export function pluginIdFromPath(path: string): string | null {
  return path.match(/\/plugins\/([^/]+)\/ui\/index\.tsx$/)?.[1] ?? null;
}

/** The discovered candidates, id → importer. */
export function discoveredModules(): Record<string, () => Promise<unknown>> {
  const modules: Record<string, () => Promise<unknown>> = {};
  for (const [path, importer] of Object.entries(UI_MODULES)) {
    const id = pluginIdFromPath(path);
    if (id) modules[id] = importer;
  }
  return modules;
}

/** Overridable collaborators; the defaults are the real ones. Tests inject both
 * because neither `fetch` nor the repo's plugin folders belong in a unit test. */
export interface LoaderDeps {
  /** The Host roster that gates the candidates. */
  list?: () => Promise<PluginInfo[]>;
  /** Candidate UI modules, id → importer. */
  modules?: Record<string, () => Promise<unknown>>;
}

/** Live plugins, id → disposer (a no-op when the factory returned nothing). */
const loaded = new Map<string, () => void>();

/** The api object handed to one module. Everything is closed over the plugin's
 * identity, so a module cannot address another plugin's routes, pushes, or slot
 * attribution. */
function makeApi(info: PluginInfo, disposers: (() => void)[]): PluginApi {
  return {
    id: info.id,
    label: info.label,
    slots: {
      register(slot: SlotName, component: SlotComponent) {
        const off = registerSlot(info.id, info.label, slot, component);
        disposers.push(off);
        return off;
      },
    },
    fetch: (path: string, init?: RequestInit) => pluginFetch(info.id, path, init),
    onPush(topic: string, handler: (data: unknown) => void) {
      const off = subscribePush(info.id, topic, handler);
      disposers.push(off);
      return off;
    },
  };
}

function detail(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** Import and initialise one plugin's UI. Never throws: a failure is recorded
 * and reported, and the plugin is left unmounted. */
async function loadOne(info: PluginInfo, importModule: () => Promise<unknown>): Promise<void> {
  // Re-loading an already-live plugin replaces it rather than doubling it.
  if (loaded.has(info.id)) teardownPlugin(info.id);

  const disposers: (() => void)[] = [];
  try {
    const module = (await importModule()) as { default?: unknown };
    const factory = module?.default;
    if (typeof factory !== "function") {
      throw new Error("UI module has no default export function");
    }
    const dispose = (factory as PluginFactory)(makeApi(info, disposers));
    loaded.set(info.id, () => {
      if (typeof dispose === "function") dispose();
      for (const off of disposers.splice(0).reverse()) off();
    });
  } catch (error) {
    // Undo any partial registration the factory managed before throwing, so a
    // half-initialised plugin never leaves a component mounted.
    for (const off of disposers.splice(0).reverse()) off();
    unregisterPlugin(info.id);
    recordLoadFailure({ id: info.id, label: info.label, detail: detail(error) });
    // eslint-disable-next-line no-console
    console.warn(`hirsel plugin "${info.id}" UI failed to load:`, error);
  }
}

/** Tear one plugin down: its own disposer, then every registration it made. */
export function teardownPlugin(id: string): void {
  const dispose = loaded.get(id);
  loaded.delete(id);
  try {
    dispose?.();
  } catch (error) {
    // eslint-disable-next-line no-console
    console.warn(`hirsel plugin "${id}": disposer threw`, error);
  }
  unregisterPlugin(id);
}

/** Read the Host roster, then initialise the UI of every enabled plugin that
 * ships one, in roster order so slot ordering is the Host's to decide. */
export async function loadPlugins(deps: LoaderDeps = {}): Promise<void> {
  const list = deps.list ?? fetchPlugins;
  const modules = deps.modules ?? discoveredModules();

  let plugins: PluginInfo[];
  try {
    plugins = await list();
  } catch (error) {
    // No plugin surface (older Host) or the Host is unreachable: no plugin UI,
    // and nothing else changes.
    // eslint-disable-next-line no-console
    console.warn("hirsel plugins: roster unavailable", error);
    return;
  }

  for (const info of plugins) {
    // A disabled plugin's UI must not register — its folder being compiled in
    // is a build fact, not the Owner's decision. An `errored` daemon keeps its
    // UI: the surface stays visible (and says so in Settings) while the Host
    // restarts it, rather than vanishing and reappearing.
    if (info.state === "disabled") continue;
    const importModule = modules[info.id];
    if (!importModule) continue; // A plugin with no UI half — nothing to mount.
    await loadOne(info, importModule);
  }
}

let started = false;

/** Load plugin UI exactly once per page load. Called when the socket first
 * authenticates — the roster and every plugin route need the same owner token
 * the `hello` frame carried, and a reconnect must not mount every component a
 * second time. */
export function startPlugins(deps: LoaderDeps = {}): void {
  if (started) return;
  started = true;
  void loadPlugins(deps);
}

/** Test seam: forget every loaded plugin and the once-per-load latch. */
export function resetPluginLoader(): void {
  for (const id of [...loaded.keys()]) teardownPlugin(id);
  started = false;
}
