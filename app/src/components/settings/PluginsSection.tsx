// Settings → Plugins: every installed plugin, its run state, its on/off switch,
// and the settings fields it declares.
//
// Host-backed like Models, but over HTTP rather than the socket, so this section
// owns its own fetch/refresh cycle: the Host is authoritative for `state` and
// `values`, and every write is followed by a re-read instead of an optimistic
// guess. The section self-hides when the Host reports no plugins (or has no
// plugin surface at all), so an older Host collapses it to nothing.
import { LoaderCircle } from "lucide-solid";
import { createSignal, For, type JSX, onMount, Show } from "solid-js";
import { fetchPlugins, savePluginSettings, setPluginEnabled } from "../../plugins/host";
import { loadFailures } from "../../plugins/registry";
import {
  type PluginInfo,
  type PluginSettingSpec,
  type PluginSettingValue,
  SECRET_SET,
} from "../../plugins/types";
import { toast } from "../../lib/toast";
import { Button } from "../ui/button";
import { Input } from "../ui/input";
import { Group, Field, SectionHeader, Toggle } from "./rows";

/** The one-word run state, coloured like the rest of the app: success green for
 * running, muted for off, and the error red only for a genuine failure. */
function StateBadge(props: { state: PluginInfo["state"] }) {
  const text = () =>
    props.state === "running" ? "Running" : props.state === "disabled" ? "Off" : "Error";
  return (
    <span
      class="shrink-0 text-xs font-medium"
      classList={{
        "text-status-success": props.state === "running",
        "text-muted-foreground": props.state === "disabled",
        "text-destructive": props.state === "errored",
      }}
    >
      {text()}
    </span>
  );
}

/** A plugin's declared settings form. Drafts are local until Save; a `secret`
 * whose stored value the Host reports as `"<set>"` renders as an empty field
 * with a "stored" placeholder and is sent ONLY when the Owner types a new one,
 * so saving an unrelated field can never blank a credential. */
function SettingsForm(props: { plugin: PluginInfo; onSaved: () => void }) {
  const initial = (spec: PluginSettingSpec): PluginSettingValue => {
    const value = props.plugin.values[spec.key];
    if (spec.kind === "boolean") {
      if (typeof value === "boolean") return value;
      return typeof spec.default === "boolean" ? spec.default : false;
    }
    if (spec.kind === "secret") return "";
    if (typeof value === "string") return value;
    return typeof spec.default === "string" ? spec.default : "";
  };

  const [draft, setDraft] = createSignal<Record<string, PluginSettingValue>>(
    Object.fromEntries(props.plugin.settings.map((spec) => [spec.key, initial(spec)])),
  );
  const [saving, setSaving] = createSignal(false);

  const secretStored = (spec: PluginSettingSpec) =>
    spec.kind === "secret" && props.plugin.values[spec.key] === SECRET_SET;

  function set(key: string, value: PluginSettingValue) {
    setDraft((current) => ({ ...current, [key]: value }));
  }

  async function save() {
    setSaving(true);
    try {
      const values: Record<string, PluginSettingValue> = {};
      for (const spec of props.plugin.settings) {
        const value = draft()[spec.key];
        // An untouched secret is omitted — never echoed back as the sentinel.
        if (spec.kind === "secret" && (value === "" || value === null)) continue;
        values[spec.key] = value;
      }
      await savePluginSettings(props.plugin.id, values);
      toast(`${props.plugin.label} settings saved`);
      props.onSaved();
    } catch (error) {
      toast(error instanceof Error ? error.message : "Couldn't save settings");
    } finally {
      setSaving(false);
    }
  }

  return (
    <Show when={props.plugin.settings.length > 0}>
      <div class="border-t border-border py-3">
        <For each={props.plugin.settings}>
          {(spec) => (
            <div class="mb-2.5 flex items-center gap-3 last:mb-0">
              <div class="min-w-0 flex-1">
                <Field title={spec.label} />
              </div>
              <Show
                when={spec.kind !== "boolean"}
                fallback={
                  <Toggle
                    ariaLabel={`${props.plugin.label}: ${spec.label}`}
                    checked={draft()[spec.key] === true}
                    disabled={saving()}
                    onChange={(value) => set(spec.key, value)}
                  />
                }
              >
                <Input
                  type={spec.kind === "secret" ? "password" : "text"}
                  class="h-9 w-[11rem] shrink-0 text-sm"
                  aria-label={`${props.plugin.label}: ${spec.label}`}
                  placeholder={secretStored(spec) ? "Stored — type to replace" : undefined}
                  disabled={saving()}
                  value={typeof draft()[spec.key] === "string" ? (draft()[spec.key] as string) : ""}
                  onInput={(e) => set(spec.key, e.currentTarget.value)}
                />
              </Show>
            </div>
          )}
        </For>
        <div class="mt-3 flex items-center gap-2">
          <Button
            size="sm"
            class="h-9"
            // Named per plugin: several plugins can show a Save at once, so the
            // bare word would be ambiguous to a screen reader.
            aria-label={`Save ${props.plugin.label} settings`}
            disabled={saving()}
            onClick={() => void save()}
          >
            Save
          </Button>
          <Show when={saving()}>
            <LoaderCircle
              class="size-3.5 animate-spin text-muted-foreground"
              aria-label="Saving"
            />
          </Show>
        </div>
      </div>
    </Show>
  );
}

function PluginRow(props: { plugin: PluginInfo; onChanged: () => void }) {
  const [busy, setBusy] = createSignal(false);
  /** A bundle listed as running whose UI never imported — visible here rather
   * than only in the console, because "enabled but nothing rendered" is
   * otherwise indistinguishable from "this plugin has no UI". */
  const loadFailure = () => loadFailures().find((f) => f.id === props.plugin.id);

  async function toggle(enabled: boolean) {
    setBusy(true);
    try {
      await setPluginEnabled(props.plugin.id, enabled);
      props.onChanged();
    } catch (error) {
      toast(error instanceof Error ? error.message : "Couldn't change plugin");
    } finally {
      setBusy(false);
    }
  }

  return (
    <div data-slot="plugin-row" data-plugin={props.plugin.id}>
      <div class="flex items-center gap-3 py-3">
        <div class="min-w-0 flex-1">
          <div class="flex items-center gap-2">
            <span class="truncate text-sm text-foreground">{props.plugin.label}</span>
            <span class="shrink-0 font-mono text-[0.7rem] text-muted-foreground">
              {props.plugin.version}
            </span>
          </div>
          <Show when={props.plugin.state === "errored" && props.plugin.error}>
            <p class="mt-0.5 text-xs leading-snug text-destructive">{props.plugin.error}</p>
          </Show>
          <Show when={loadFailure()}>
            {(failure) => (
              <p class="mt-0.5 text-xs leading-snug text-destructive">
                UI failed to load: {failure().detail}
              </p>
            )}
          </Show>
        </div>
        <StateBadge state={props.plugin.state} />
        <Toggle
          ariaLabel={`Enable ${props.plugin.label}`}
          checked={props.plugin.state !== "disabled"}
          disabled={busy()}
          onChange={(enabled) => void toggle(enabled)}
        />
      </div>
      <SettingsForm plugin={props.plugin} onSaved={props.onChanged} />
    </div>
  );
}

export function PluginsSection(): JSX.Element {
  const [plugins, setPlugins] = createSignal<PluginInfo[] | null>(null);

  async function refresh() {
    try {
      setPlugins(await fetchPlugins());
    } catch {
      // No plugin surface (older Host) or the Host is unreachable: the section
      // stays hidden rather than shouting about an absent optional feature.
      setPlugins(null);
    }
  }

  onMount(() => void refresh());

  return (
    <Show when={(plugins()?.length ?? 0) > 0}>
      <SectionHeader id="settings-plugins">Plugins</SectionHeader>
      <Group class="divide-y divide-border">
        <For each={plugins() ?? []}>
          {(plugin) => <PluginRow plugin={plugin} onChanged={() => void refresh()} />}
        </For>
      </Group>
    </Show>
  );
}
