import { Settings as SettingsIcon } from "lucide-solid";
import { createSignal, onMount, Show } from "solid-js";
import { resolveWsUrl } from "../../lib/endpoint";
import {
  createFocusTrap,
  createMediaFlag,
  phoneUtilityRestoreTarget,
} from "../../lib/focus";
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
import {
  computeFingerprint,
  copyText,
  DEBUG_KEY,
  DEVICE_LABEL_KEY,
  PHASE_WORD,
  readLocal,
} from "./prefs";

/** At/above `rail` Settings is an in-flow right-region inspector (Tab stays
 * free, non-modal); below it a full-screen modal sheet whose Tab is trapped. */
const RAIL_MQ = "(min-width: 1100px)";

function SettingsPanel() {
  const endpoint = resolveWsUrl();

  const [deviceLabel, setDeviceLabel] = createSignal(readLocal(DEVICE_LABEL_KEY));
  const [debug, setDebug] = createSignal(readLocal(DEBUG_KEY) === "1");
  const [fingerprint, setFingerprint] = createSignal("…");
  const [confirmForget, setConfirmForget] = createSignal(false);

  let panelRef: HTMLDivElement | undefined;
  const phone = createMediaFlag("(max-width: 1099.98px)");

  onMount(() => {
    void computeFingerprint(getStoredToken()).then(setFingerprint);
    // Focus management (C21): full-screen modal sheet on phone (trap Tab so the
    // chat behind stays out of the tab order), in-flow inspector at `rail`
    // (leave Tab free). Escape returns the right region to Pings; when the
    // Forget-token dialog is up it sits on top of the trap stack and owns Escape.
    createFocusTrap(() => panelRef, {
      onEscape: closeRightRegion,
      trapTab: () => !window.matchMedia(RAIL_MQ).matches,
      restoreTo: () => phone() ? phoneUtilityRestoreTarget() : undefined,
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
    // Same responsive presentation as ProcessesPanel — phone: a full-screen
    // modal `fixed` sheet; desktop (`rail`): an in-flow
    // right-edge inspector inside ChatView's row, one exclusive slot, never over
    // the chat measure on the left.
    <div
      ref={(node) => { panelRef = node; }}
      tabindex={-1}
      data-slot="settings-panel"
      role={phone() ? "dialog" : "complementary"}
      aria-modal={phone() ? "true" : undefined}
      aria-labelledby="settings-pane-title"
      class="fixed inset-0 z-40 flex flex-col bg-background outline-none pb-[env(safe-area-inset-bottom)]
        rail:relative rail:inset-auto rail:z-auto rail:min-h-0 rail:w-[clamp(340px,38vw,440px)] rail:shrink-0 rail:border-l rail:border-border rail:pb-0
        motion-safe:animate-in motion-safe:fade-in motion-safe:slide-in-from-bottom motion-safe:duration-200
        motion-safe:rail:slide-in-from-bottom-0 motion-safe:rail:slide-in-from-right-2 motion-safe:rail:duration-150"
    >
      {/* ONE header at both widths — see PaneHeader. Settings is summoned and
          dismissed; the × is the whole exit vocabulary, at every width. */}
      <PaneHeader
        icon={<SettingsIcon class="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />}
        title="Settings"
        titleId="settings-pane-title"
        onClose={closeRightRegion}
        closeLabel="Close Settings"
      />

      {/* Block flow (not a flex column): as a flex item this scroll region can
          shrink to the available height (min-h-0) and scroll, while its groups
          keep their natural height instead of compressing to fit. */}
      <div class="thin-scrollbar min-h-0 flex-1 overflow-y-auto px-4 pt-4 pb-8">
        <AppearanceSection />
        <ModelsSection />
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
            then whatever settings UI the plugins themselves contribute. Last in
            the pane — the app's own settings stay above third-party surface. */}
        <PluginsSection />
        <div class="mt-6 flex flex-col gap-2.5 empty:hidden">
          <PluginSlot name="settings.section" />
        </div>
      </div>

      <Show when={confirmForget()}>
        <ConfirmForgetDialog onConfirm={forgetToken} onCancel={() => setConfirmForget(false)} />
      </Show>
    </div>
  );
}

/** Settings surface (single-owner right region / desktop-shell): an in-flow
 * inspector reachable from the NavRail gear or the phone header overflow (a
 * full-screen modal sheet on phone). Grouped calm-terminal sections —
 * Appearance, Models, Connection & devices, Notifications, Device label &
 * identity, About & debug — each in its own module beside this one, mirroring
 * the Android Settings screen and adapted honestly to the browser WS client's
 * actual capabilities. Mounted inside ChatView's row so the desktop aside
 * resolves against the frame, not the viewport. */
export function SettingsSheet() {
  return (
    <Show when={state.rightRegion === "settings"}>
      <SettingsPanel />
    </Show>
  );
}
