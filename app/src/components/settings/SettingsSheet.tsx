import { Settings as SettingsIcon } from "lucide-solid";
import { createSignal, onMount, Show } from "solid-js";
import { resolveWsUrl } from "../../lib/endpoint";
import { createFocusTrap, phoneUtilityRestoreTarget } from "../../lib/focus";
import { showAgentCode } from "../../lib/prefs";
import { themeMode } from "../../lib/theme";
import { toast } from "../../lib/toast";
import { APP_VERSION } from "../../lib/version";
import { clearSettingsScrollTarget, closeRightRegion, state } from "../../store/store";
import { clearStoredToken, getStoredToken } from "../../ws/client";
import { PaneHeader } from "../ui/PaneHeader";
import { PluginSlot } from "../../plugins/PluginSlot";
import { AboutSection } from "./AboutSection";
import { AppearanceSection } from "./AppearanceSection";
import { ConfirmForgetDialog, ConnectionSection } from "./ConnectionSection";
import { IdentitySection } from "./IdentitySection";
import { ModelsSection } from "./ModelsSection";
import { NotificationsSection } from "./NotificationsSection";
import { PluginsSection } from "./PluginsSection";
import { PromptSection } from "./PromptSection";
import {
  computeFingerprint,
  copyText,
  DEBUG_KEY,
  DEVICE_LABEL_KEY,
  PHASE_WORD,
  readLocal,
} from "./prefs";

/** The one centred column Settings reads in: the task world's own reading
 * measure plus a gutter each side (`--container-frame`). Settings is a deep,
 * form-shaped visit — grouped rows, a device fingerprint, monospace prompt
 * editors — and the 340–440px inspector rail it used to dock in wrapped every
 * one of them. It does NOT get a bespoke width: the app has exactly one
 * horizontal rhythm and a full-viewport utility centres on it like everything
 * else. */
const SETTINGS_COLUMN = "mx-auto w-full max-w-frame px-gutter";

function SettingsPanel() {
  const endpoint = resolveWsUrl();

  const [deviceLabel, setDeviceLabel] = createSignal(readLocal(DEVICE_LABEL_KEY));
  const [debug, setDebug] = createSignal(readLocal(DEBUG_KEY) === "1");
  const [fingerprint, setFingerprint] = createSignal("…");
  const [confirmForget, setConfirmForget] = createSignal(false);

  let panelRef: HTMLDivElement | undefined;

  onMount(() => {
    void computeFingerprint(getStoredToken()).then(setFingerprint);
    // ONE focus contract at every width now that Settings is full-viewport
    // everywhere: a true modal — Tab trapped so the task world behind it stays
    // out of the tab order, Escape dismissing it and NOTHING else (the trap
    // consumes the key in the capture phase, so keymap's Esc ladder never
    // advances to clearing task focus on the same keystroke), and focus
    // restored to the standing ⋯ that summoned it. That restoration target is
    // deliberate rather than "whatever was focused": the menu item that opened
    // Settings has unmounted by the time the panel closes, so the captured
    // element is a detached node and focus would land nowhere at all.
    createFocusTrap(() => panelRef, {
      onEscape: closeRightRegion,
      restoreTo: phoneUtilityRestoreTarget,
    });
    // Consume a one-shot scroll target (spec item 6): the phone overflow "Model
    // settings" row opens Settings pointed at the Models section — an honest
    // affordance instead of silently landing on Appearance. Deferred a
    // microtask so the panel + its scroll region have laid out first.
    const target = state.settingsScrollTarget;
    if (target === "models") {
      queueMicrotask(() =>
        document
          .getElementById("settings-models")
          ?.scrollIntoView({ block: "start", behavior: "auto" }),
      );
    }
    clearSettingsScrollTarget();
  });

  function saveLabel(trimmed: string) {
    try {
      localStorage.setItem(DEVICE_LABEL_KEY, trimmed);
    } catch {
      /* best-effort */
    }
    setDeviceLabel(trimmed);
    toast("Device label saved");
  }

  function toggleDebug(v: boolean) {
    setDebug(v);
    try {
      localStorage.setItem(DEBUG_KEY, v ? "1" : "0");
    } catch {
      /* best-effort */
    }
  }

  function diagnostics(): string {
    return [
      "hirsel diagnostics",
      `app version: ${APP_VERSION}`,
      `host version: ${state.hostVersion ?? "not reported"}`,
      `endpoint: ${endpoint}`,
      `connection: ${PHASE_WORD[state.connection]}`,
      `theme: ${themeMode()}`,
      "notifications: not available (web)",
      `debug: ${debug() ? "on" : "off"}`,
      `show agent code: ${showAgentCode() ? "on" : "off"}`,
      `device label: ${deviceLabel() || "(unset)"}`,
      `identity: ${fingerprint()}`,
      `user agent: ${navigator.userAgent}`,
    ].join("\n");
  }

  function forgetToken() {
    clearStoredToken();
    location.reload();
  }

  return (
    // ONE presentation at every width: a full-viewport modal overlay on the
    // canvas. Settings is an infrequent, deep, form-shaped visit — unlike
    // Processes, which is a glance kept BESIDE the work and stays a docked
    // inspector — so it takes the screen for the length of that visit and gives
    // its rows, its fingerprints and its monospace prompt editors a real
    // reading measure instead of a 340px rail. It is still summoned, still
    // trapped, and still returns the Owner to the exact task they left.
    <div
      ref={(node) => { panelRef = node; }}
      tabindex={-1}
      data-slot="settings-panel"
      role="dialog"
      aria-modal="true"
      aria-labelledby="settings-pane-title"
      /* One 200ms fade + 8px settle, the same restrained continuity motion the
         two fields swap with (DESIGN §5). Deliberately NOT the phone sheet's
         full slide-up any more: this is no longer a panel rising over the
         world at one width and docking at another — it IS the surface for the
         length of the visit, and a full-viewport slide is a gesture the calm
         voice doesn't need. */
      class="fixed inset-0 z-40 flex flex-col bg-background outline-none pb-[env(safe-area-inset-bottom)]
        motion-safe:animate-in motion-safe:fade-in motion-safe:slide-in-from-bottom-2 motion-safe:duration-200"
    >
      {/* ONE header at every width — see PaneHeader. Settings is summoned and
          dismissed; the × is the whole exit vocabulary. Its row centres on the
          same column as the content below it, so the title and the × sit on the
          content's own edges rather than in the far corners of the viewport. */}
      <PaneHeader
        icon={<SettingsIcon class="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />}
        title="Settings"
        titleId="settings-pane-title"
        onClose={closeRightRegion}
        closeLabel="Close Settings"
        contentClass={SETTINGS_COLUMN}
      />

      {/* Block flow (not a flex column): as a flex item this scroll region can
          shrink to the available height (min-h-0) and scroll, while its groups
          keep their natural height instead of compressing to fit. */}
      <div class="thin-scrollbar min-h-0 flex-1 overflow-y-auto">
        <div data-slot="settings-column" class={`${SETTINGS_COLUMN} pt-6 pb-16`}>
          <AppearanceSection />
          <ModelsSection />
          <PromptSection />
          <ConnectionSection
            endpoint={endpoint}
            deviceLabel={deviceLabel()}
            onForget={() => setConfirmForget(true)}
          />
          <NotificationsSection />
          <IdentitySection
            deviceLabel={deviceLabel()}
            fingerprint={fingerprint()}
            onSaveLabel={saveLabel}
          />
          <AboutSection
            debug={debug()}
            onDebugChange={toggleDebug}
            onCopyDiagnostics={() => copyText(diagnostics(), "diagnostics")}
          />
          {/* Plugins: the Host-backed roster (state, on/off, declared settings),
              then whatever settings UI the plugins themselves contribute. Last
              in the pane — the app's own settings stay above third-party
              surface. */}
          <PluginsSection />
          <div class="mt-6 flex flex-col gap-2.5 empty:hidden">
            <PluginSlot name="settings.section" />
          </div>
        </div>
      </div>

      <Show when={confirmForget()}>
        <ConfirmForgetDialog onConfirm={forgetToken} onCancel={() => setConfirmForget(false)} />
      </Show>
    </div>
  );
}

/** Settings surface (single-owner right region): a full-viewport modal overlay
 * at every width, summoned from the standing ⋯ or `g s`. Grouped calm-terminal
 * sections — Appearance, Models, Connection & devices, Notifications, Device
 * label & identity, About & debug — each in its own module beside this one,
 * mirroring the Android Settings screen and adapted honestly to the browser WS
 * client's actual capabilities. It owns the right region enum like the docked
 * panes do (so summoning it still evicts Processes or Canvas), but it takes no
 * room in that row: it is `fixed`, over the whole world. */
export function SettingsSheet() {
  return (
    <Show when={state.rightRegion === "settings"}>
      <SettingsPanel />
    </Show>
  );
}
