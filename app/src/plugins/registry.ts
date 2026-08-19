// The plugin registry: which components are mounted in which slot, which
// `plugin_push` topics have listeners, and which bundles failed to load.
//
// This is deliberately NOT part of the app store. The reducer owns protocol
// truth that the app renders; plugin contributions are foreign code the app only
// hosts, so they live in their own reactive island. Nothing here can make the
// reducer produce a different AppState.
import { createStore, produce } from "solid-js/store";
import type { PluginPushMsg } from "../protocol";
import { SLOT_NAMES, type SlotComponent, type SlotName } from "./types";

/** One mounted contribution. `pluginId`/`label` are carried so an error
 * boundary can name the culprit instead of showing an anonymous failure. */
export interface SlotEntry {
  pluginId: string;
  label: string;
  component: SlotComponent;
}

type SlotTable = Record<SlotName, SlotEntry[]>;

function emptySlots(): SlotTable {
  return {
    "settings.section": [],
    "task.panel": [],
    "home.section": [],
  };
}

const [slots, setSlots] = createStore<SlotTable>(emptySlots());

/** Reactive read of everything mounted in one slot, in registration order. */
export function slotEntries(name: SlotName): SlotEntry[] {
  return slots[name] ?? [];
}

/** Mount a component. Returns an idempotent unregister. */
export function registerSlot(
  pluginId: string,
  label: string,
  name: SlotName,
  component: SlotComponent,
): () => void {
  if (!SLOT_NAMES.includes(name)) {
    throw new Error(`unknown plugin slot: ${String(name)}`);
  }
  const entry: SlotEntry = { pluginId, label, component };
  setSlots(name, (current) => [...current, entry]);
  return () => {
    setSlots(name, (current) => current.filter((candidate) => candidate !== entry));
  };
}

// ---- plugin_push fan-out ---------------------------------------------------

type PushHandler = (data: unknown) => void;

/** pluginId → topic → live handlers. Nested maps rather than a composite string
 * key so no id or topic can ever collide through the separator, and so a
 * plugin's whole subtree drops in one delete. A plain Map, not a store:
 * delivering a push must not itself be a reactive write. */
const pushHandlers = new Map<string, Map<string, Set<PushHandler>>>();

export function subscribePush(
  pluginId: string,
  topic: string,
  handler: PushHandler,
): () => void {
  let topics = pushHandlers.get(pluginId);
  if (!topics) {
    topics = new Map();
    pushHandlers.set(pluginId, topics);
  }
  let handlers = topics.get(topic);
  if (!handlers) {
    handlers = new Set();
    topics.set(topic, handlers);
  }
  handlers.add(handler);
  return () => {
    const liveTopics = pushHandlers.get(pluginId);
    const live = liveTopics?.get(topic);
    if (!liveTopics || !live) return;
    live.delete(handler);
    if (live.size === 0) liveTopics.delete(topic);
    if (liveTopics.size === 0) pushHandlers.delete(pluginId);
  };
}

/** Route one `plugin_push` frame. A frame nobody subscribed to is dropped
 * silently — plugins come and go, and an unheard push is not an app error. One
 * handler throwing never stops the others or the socket. */
export function deliverPluginPush(frame: PluginPushMsg): void {
  const handlers = pushHandlers.get(frame.plugin)?.get(frame.topic);
  if (!handlers) return;
  for (const handler of [...handlers]) {
    try {
      handler(frame.data);
    } catch (error) {
      // eslint-disable-next-line no-console
      console.warn(`hirsel plugin "${frame.plugin}": push handler threw`, error);
    }
  }
}

// ---- load failures ---------------------------------------------------------

export interface PluginLoadFailure {
  id: string;
  label: string;
  detail: string;
}

const [failures, setFailures] = createStore<{ list: PluginLoadFailure[] }>({ list: [] });

/** Record that a bundle could not be imported or initialised. Surfaced in
 * Settings → Plugins so a broken bundle is visible rather than merely absent. */
export function recordLoadFailure(failure: PluginLoadFailure): void {
  setFailures(
    produce((current) => {
      const existing = current.list.findIndex((f) => f.id === failure.id);
      if (existing >= 0) current.list[existing] = failure;
      else current.list.push(failure);
    }),
  );
}

export function clearLoadFailure(id: string): void {
  setFailures("list", (current) => current.filter((f) => f.id !== id));
}

/** Reactive read of the bundles that failed this session. */
export function loadFailures(): PluginLoadFailure[] {
  return failures.list;
}

// ---- teardown --------------------------------------------------------------

/** Drop every contribution and subscription belonging to one plugin. The loader
 * calls this before re-initialising a bundle, so nothing is ever mounted twice.
 */
export function unregisterPlugin(id: string): void {
  for (const name of SLOT_NAMES) {
    setSlots(name, (current) => current.filter((entry) => entry.pluginId !== id));
  }
  pushHandlers.delete(id);
  clearLoadFailure(id);
}

/** Full reset — used by tests, which import a fresh module graph per case. */
export function resetPluginRegistry(): void {
  setSlots(emptySlots());
  pushHandlers.clear();
  setFailures("list", []);
}
