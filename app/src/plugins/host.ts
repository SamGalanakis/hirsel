// The plugin tier's HTTP calls, one function per documented endpoint. Every
// caller in the app goes through here so the endpoint shapes live in exactly one
// place (PROTOCOL.md "Plugin tier"). A plugin's OWN routes are not here — those
// are the plugin's business, reached through `api.fetch`.
import { apiFetch, apiJson } from "../lib/api";
import type { PluginInfo, PluginListResponse, PluginSettingValue } from "./types";

/** Every installed plugin, with its run state and settings descriptors. */
export async function fetchPlugins(): Promise<PluginInfo[]> {
  const body = await apiJson<PluginListResponse>("/api/plugins");
  return body.plugins ?? [];
}

/** Enable or disable a plugin. The Host owns the resulting state; callers
 * re-read `fetchPlugins()` rather than guessing it. */
export async function setPluginEnabled(id: string, enabled: boolean): Promise<void> {
  await apiJson<unknown>(`/api/plugins/${encodeURIComponent(id)}/enabled`, {
    method: "POST",
    body: { enabled },
  });
}

/** Write settings values. Only the keys present are changed — a `secret` the
 * Owner did not retype is omitted, never sent back as the `"<set>"` sentinel. */
export async function savePluginSettings(
  id: string,
  values: Record<string, PluginSettingValue>,
): Promise<void> {
  await apiJson<unknown>(`/api/plugins/${encodeURIComponent(id)}/settings`, {
    method: "POST",
    body: { values },
  });
}

/** The authenticated fetch handed to one plugin's UI, rooted at that plugin's
 * Host router. Defined here so the route prefix has one definition. */
export function pluginFetch(id: string, path: string, init?: RequestInit): Promise<Response> {
  const suffix = path.startsWith("/") ? path : `/${path}`;
  return apiFetch(`/api/plugins/${encodeURIComponent(id)}${suffix}`, init);
}
