import { ChevronLeft, Copy, X } from "lucide-solid";
import { createSignal, For, type JSX, onMount, Show } from "solid-js";
import { resolveWsUrl } from "../../lib/endpoint";
import { createFocusTrap } from "../../lib/focus";
import { cn } from "../../lib/utils";
import { setThemeMode, themeMode } from "../../lib/theme";
import { toast } from "../../lib/toast";
import { APP_VERSION } from "../../lib/version";
import { setSettingsOpen, state } from "../../store/store";
import type { ConnectionStatus } from "../../store/types";
import { clearStoredToken, getStoredToken } from "../../ws/client";
import { ConnectionPill } from "../ConnectionPill";
import { Button } from "../ui/button";
import { Input } from "../ui/input";

// Local-only client preferences. The WS protocol (see PROTOCOL.md) carries only
// a token + replay cursor from this client — no device label, no roster, no
// pairing, no push. So these are honestly scoped to this browser: a cosmetic
// display label and a debug flag, both persisted locally and surfaced in the
// diagnostics blob, never sent to the Host.
const DEVICE_LABEL_KEY = "hirsel.deviceLabel";
const DEBUG_KEY = "hirsel.debug";

/** At/above `rail` Settings is a right-docked inspector (Tab stays free); below
 * it a full-screen sheet whose Tab is trapped (C21). */
const RAIL_MQ = "(min-width: 1100px)";

const PHASE_WORD: Record<ConnectionStatus, string> = {
  connecting: "connecting…",
  connected: "connected",
  reconnecting: "reconnecting…",
};

function readLocal(key: string): string {
  try {
    return localStorage.getItem(key) ?? "";
  } catch {
    return "";
  }
}

/** A one-way, secret-free fingerprint of this browser's access token — safe to
 * show and copy (mirrors the Android client's iroh-identity fingerprint). */
async function computeFingerprint(secret: string | null): Promise<string> {
  if (!secret) return "unavailable";
  try {
    const bytes = new TextEncoder().encode(secret);
    const digest = await crypto.subtle.digest("SHA-256", bytes);
    const hex = [...new Uint8Array(digest)]
      .slice(0, 10)
      .map((b) => b.toString(16).padStart(2, "0"))
      .join("")
      .toUpperCase();
    return hex.match(/.{1,4}/g)?.join(" ") ?? hex;
  } catch {
    return "unavailable";
  }
}

/** Show the token exists without revealing it: first/last two chars, the rest
 * dotted. Copying a bearer secret is deliberately not offered. */
function maskToken(token: string | null): string {
  if (!token) return "none stored";
  if (token.length <= 6) return "•".repeat(token.length);
  return `${token.slice(0, 2)}${"•".repeat(Math.min(12, token.length - 4))}${token.slice(-2)}`;
}

function copyText(value: string, label: string): void {
  const ok = () => toast(`Copied ${label}`);
  const fail = () => toast("Couldn't copy", { variant: "error" });
  try {
    if (navigator.clipboard?.writeText) {
      navigator.clipboard.writeText(value).then(ok).catch(fail);
      return;
    }
  } catch {
    /* fall through */
  }
  fail();
}

// ── Small building blocks ──────────────────────────────────────────────────

/** Grouped card — a raised white-paper (light) / step-up (dark) surface with a
 * hairline border, mirroring the Android SettingsCard. */
function Card(props: { children: JSX.Element; class?: string }) {
  return (
    <div class={cn("overflow-hidden rounded-xl border border-border bg-card", props.class)}>
      {props.children}
    </div>
  );
}

function SectionHeader(props: { children: JSX.Element }) {
  return (
    <h2 class="mt-6 mb-2 px-1 text-[0.68rem] font-medium uppercase tracking-[0.06em] text-muted-foreground first:mt-0">
      {props.children}
    </h2>
  );
}

/** A title + optional one-line subtitle, the row's text column. */
function Field(props: { title: string; subtitle?: string }) {
  return (
    <div class="flex min-w-0 flex-col">
      <span class="text-sm text-foreground">{props.title}</span>
      <Show when={props.subtitle}>
        <span class="mt-0.5 text-xs leading-snug text-muted-foreground">{props.subtitle}</span>
      </Show>
    </div>
  );
}

/** Neutral segmented control (System / Light / Dark, notify scope). Selection
 * is a raised neutral pill, never indigo — per DESIGN, indigo stays reserved
 * for "attend to this," so a chosen segment reads as a lift, not an alert. */
function SegmentedControl<T extends string>(props: {
  options: { value: T; label: string }[];
  value: T;
  onChange: (value: T) => void;
  ariaLabel: string;
}) {
  return (
    <div
      role="radiogroup"
      aria-label={props.ariaLabel}
      class="flex items-center gap-1 rounded-lg border border-border bg-secondary p-1"
    >
      <For each={props.options}>
        {(opt) => {
          const selected = () => props.value === opt.value;
          return (
            <button
              type="button"
              role="radio"
              aria-checked={selected()}
              class="flex-1 rounded-md px-2 py-1.5 text-[0.8125rem] outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring"
              classList={{
                "bg-card font-medium text-foreground shadow-[0_1px_2px_0_rgb(0_0_0/0.06)]":
                  selected(),
                "text-muted-foreground hover:text-foreground": !selected(),
              }}
              onClick={() => props.onChange(opt.value)}
            >
              {opt.label}
            </button>
          );
        }}
      </For>
    </div>
  );
}

/** Accessible on/off switch. On-state track is the indigo primary (an
 * interactive control state, matching the Android switch), thumb near-white so
 * it reads in both themes. */
function Toggle(props: { checked: boolean; onChange: (v: boolean) => void; ariaLabel: string }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={props.checked}
      aria-label={props.ariaLabel}
      onClick={() => props.onChange(!props.checked)}
      class="relative inline-flex h-5 w-9 shrink-0 items-center rounded-full border transition-colors outline-none focus-visible:ring-2 focus-visible:ring-ring"
      classList={{
        "border-primary bg-primary": props.checked,
        "border-input bg-secondary": !props.checked,
      }}
    >
      <span
        aria-hidden="true"
        class="pointer-events-none ml-0.5 size-3.5 rounded-full transition-transform"
        classList={{
          "translate-x-4 bg-primary-foreground": props.checked,
          "translate-x-0 bg-muted-foreground": !props.checked,
        }}
      />
    </button>
  );
}

/** A copyable value in a raised inset field (endpoint, identity fingerprint). */
function CopyRow(props: { value: string; label: string; mono?: boolean }) {
  return (
    <button
      type="button"
      onClick={() => copyText(props.value, props.label)}
      class="flex w-full items-center gap-2 rounded-lg border border-border bg-surface px-3 py-2.5 text-left outline-none transition-colors hover:border-input focus-visible:ring-2 focus-visible:ring-ring"
      aria-label={`Copy ${props.label}`}
    >
      <span
        class="min-w-0 flex-1 truncate text-[0.8125rem] text-foreground"
        classList={{ "font-mono": props.mono }}
      >
        {props.value}
      </span>
      <Copy class="size-3.5 shrink-0 text-muted-foreground" aria-hidden="true" />
    </button>
  );
}

function ConfirmForgetDialog(props: { onConfirm: () => void; onCancel: () => void }) {
  let dialogRef: HTMLDivElement | undefined;
  // Topmost modal over the Settings sheet: trap focus in the card and own
  // Escape (cancel) while it's up; the stack hands control back to the Settings
  // panel trap on close (C21).
  onMount(() => {
    createFocusTrap(() => dialogRef, { onEscape: () => props.onCancel() });
  });

  return (
    // Centered within the panel (absolute), calm dim + hairline card. A click on
    // the backdrop (not the card) cancels; Escape cancels via the focus trap.
    // eslint-disable-next-line jsx-a11y/click-events-have-key-events, jsx-a11y/no-static-element-interactions
    <div
      class="absolute inset-0 z-50 flex items-center justify-center bg-background/70 p-6"
      onClick={(e) => {
        if (e.target === e.currentTarget) props.onCancel();
      }}
    >
      <div
        ref={dialogRef}
        tabindex={-1}
        role="alertdialog"
        aria-label="Forget token"
        class="w-full max-w-[320px] rounded-xl border border-border bg-card p-4 shadow-lg outline-none"
      >
        <h3 class="m-0 text-sm font-semibold text-foreground">Forget this token?</h3>
        <p class="mt-1.5 mb-4 text-[0.8125rem] leading-relaxed text-muted-foreground">
          Clears the access token stored in this browser and reloads to the connect screen. You'll
          need the token again to reconnect. The Host and its history are untouched.
        </p>
        <div class="flex justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={props.onCancel}>
            Cancel
          </Button>
          <Button variant="destructive" size="sm" onClick={props.onConfirm}>
            Forget token
          </Button>
        </div>
      </div>
    </div>
  );
}

// ── Panel ──────────────────────────────────────────────────────────────────

function SettingsPanel() {
  const endpoint = resolveWsUrl();

  const [deviceLabel, setDeviceLabel] = createSignal(readLocal(DEVICE_LABEL_KEY));
  const [labelDraft, setLabelDraft] = createSignal(deviceLabel());
  const [debug, setDebug] = createSignal(readLocal(DEBUG_KEY) === "1");
  const [fingerprint, setFingerprint] = createSignal("…");
  const [confirmForget, setConfirmForget] = createSignal(false);

  let panelRef: HTMLDivElement | undefined;

  onMount(() => {
    void computeFingerprint(getStoredToken()).then(setFingerprint);
    // Focus management (C21): full-screen sheet on phone (trap Tab so the chat
    // behind stays out of the tab order), right-docked inspector at `rail`
    // (leave Tab free). Escape closes Settings; when the Forget-token dialog is
    // up it sits on top of the trap stack and owns Escape instead.
    createFocusTrap(() => panelRef, {
      onEscape: () => setSettingsOpen(false),
      trapTab: () => !window.matchMedia(RAIL_MQ).matches,
    });
  });

  const canSaveLabel = () => {
    const t = labelDraft().trim();
    return t.length > 0 && t !== deviceLabel();
  };

  function saveLabel() {
    const trimmed = labelDraft().trim();
    if (trimmed.length === 0 || trimmed === deviceLabel()) return;
    try {
      localStorage.setItem(DEVICE_LABEL_KEY, trimmed);
    } catch {
      /* best-effort */
    }
    setDeviceLabel(trimmed);
    setLabelDraft(trimmed);
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
      `endpoint: ${endpoint}`,
      `connection: ${PHASE_WORD[state.connection]}`,
      `theme: ${themeMode()}`,
      "notifications: not available (web)",
      `debug: ${debug() ? "on" : "off"}`,
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
    // Same responsive docking as ProcessesPanel — phone: a full-screen `fixed`
    // sheet with a back affordance; desktop (`rail`): a right-docked inspector
    // `absolute` inside ChatView's `relative` row, overlaying the right region
    // (Pings rail / Side Chat) only, never the chat measure on the left.
    <div
      ref={panelRef}
      tabindex={-1}
      data-slot="settings-panel"
      class="fixed inset-0 z-40 flex flex-col bg-background outline-none pb-[env(safe-area-inset-bottom)]
        rail:absolute rail:left-auto rail:z-30 rail:w-[360px] rail:border-l rail:border-border rail:pb-0"
    >
      <header class="flex flex-shrink-0 items-center gap-2 border-b border-border px-2 py-3 pt-[calc(env(safe-area-inset-top)+0.75rem)] rail:h-12 rail:py-0">
        <button
          type="button"
          class="flex items-center gap-0.5 rounded-md px-2 py-1 text-sm text-foreground transition-colors hover:bg-muted"
          onClick={() => setSettingsOpen(false)}
          aria-label="Close Settings"
        >
          <ChevronLeft class="size-5 rail:hidden" aria-hidden="true" />
          <X class="hidden size-4 rail:block" aria-hidden="true" />
          <span class="rail:hidden">Chat</span>
        </button>
        <h1 class="m-0 flex-1 text-center text-base font-semibold tracking-[0.01em] rail:text-left">
          Settings
        </h1>
        <span class="w-[3.25rem] rail:hidden" aria-hidden="true" />
      </header>

      {/* Block flow (not a flex column): as a flex item this scroll region can
          shrink to the available height (min-h-0) and scroll, while its cards
          keep their natural height instead of compressing to fit. */}
      <div class="thin-scrollbar min-h-0 flex-1 overflow-y-auto px-4 pt-4 pb-8">
        {/* ── Appearance ─────────────────────────────────────────────── */}
        <SectionHeader>Appearance</SectionHeader>
        <Card class="p-3.5">
          <Field title="Theme" subtitle="System follows your device's light or dark setting." />
          <div class="mt-3">
            <SegmentedControl
              ariaLabel="Theme"
              value={themeMode()}
              onChange={setThemeMode}
              options={[
                { value: "system", label: "System" },
                { value: "light", label: "Light" },
                { value: "dark", label: "Dark" },
              ]}
            />
          </div>
        </Card>

        {/* ── Connection & devices ───────────────────────────────────── */}
        <SectionHeader>Connection &amp; devices</SectionHeader>
        <Card class="divide-y divide-border">
          <div class="flex items-center justify-between gap-3 px-3.5 py-3">
            <span class="text-sm text-foreground">Status</span>
            <ConnectionPill />
          </div>
          <div class="flex items-center gap-3 px-3.5 py-3">
            <span
              aria-hidden="true"
              class="size-2 shrink-0 rounded-full"
              classList={{
                "bg-status-success": state.connection === "connected",
                "bg-status-attention motion-safe:animate-pulse": state.connection !== "connected",
              }}
            />
            <div class="min-w-0 flex-1">
              <div class="truncate text-sm font-medium text-foreground">
                {deviceLabel() || "This browser"}
              </div>
              <div class="mt-0.5 text-xs text-muted-foreground">
                This device · {PHASE_WORD[state.connection]}
              </div>
            </div>
            <span class="shrink-0 rounded-full bg-muted px-1.5 py-0.5 text-[0.65rem] font-medium text-muted-foreground">
              You
            </span>
          </div>
          <div class="px-3.5 py-3">
            <Field title="Server endpoint" subtitle="Where this client connects. Set at build time." />
            <div class="mt-2">
              <CopyRow value={endpoint} label="server endpoint" mono />
            </div>
          </div>
          <div class="flex items-center gap-3 px-3.5 py-3">
            <div class="min-w-0 flex-1">
              <div class="text-sm text-foreground">Access token</div>
              <div class="mt-0.5 font-mono text-xs text-muted-foreground">
                {maskToken(getStoredToken())}
              </div>
            </div>
            <Button
              variant="ghost"
              size="sm"
              class="shrink-0 text-status-danger hover:text-status-danger"
              onClick={() => setConfirmForget(true)}
            >
              Forget
            </Button>
          </div>
        </Card>
        <p class="mt-2 px-1 text-xs leading-snug text-muted-foreground">
          Pairing and the device roster live on the Host — the web client holds only this browser's
          token, so it can't list or unpair other devices.
        </p>

        {/* ── Notifications ──────────────────────────────────────────── */}
        <SectionHeader>Notifications</SectionHeader>
        <Card class="divide-y divide-border">
          <div class="flex items-center gap-3 px-3.5 py-3">
            <Field
              title="Tab title badge"
              subtitle="Unread Pings show as “(3) hirsel” in this browser tab."
            />
            <span class="shrink-0 rounded-full bg-status-success/15 px-2 py-0.5 text-[0.65rem] font-medium text-status-success">
              On
            </span>
          </div>
          <div class="px-3.5 py-3">
            <Field
              title="Web push"
              subtitle="Not available in this build — the web client has no push. Keep the tab open or install the PWA to catch the title badge."
            />
          </div>
        </Card>

        {/* ── Device label & identity ────────────────────────────────── */}
        <SectionHeader>Device label &amp; identity</SectionHeader>
        <Card class="p-3.5">
          <Field
            title="Device label"
            subtitle="A local name for this browser. Stored here only — the web client sends no label to the Host."
          />
          <div class="mt-2.5 flex items-center gap-2">
            <Input
              value={labelDraft()}
              onInput={(e) => setLabelDraft(e.currentTarget.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") saveLabel();
              }}
              placeholder="This browser"
              class="h-9 text-sm"
              aria-label="Device label"
            />
            <Button size="sm" class="h-9 shrink-0" disabled={!canSaveLabel()} onClick={saveLabel}>
              Save
            </Button>
          </div>
        </Card>
        <Card class="mt-2.5 p-3.5">
          <Field
            title="Device identity"
            subtitle="A stable, one-way fingerprint of this browser's access token. Safe to share."
          />
          <div class="mt-2.5">
            <CopyRow value={fingerprint()} label="identity fingerprint" mono />
          </div>
        </Card>

        {/* ── About & debug ──────────────────────────────────────────── */}
        <SectionHeader>About &amp; debug</SectionHeader>
        <Card class="divide-y divide-border">
          <div class="flex items-center justify-between gap-3 px-3.5 py-3">
            <span class="text-sm text-foreground">App version</span>
            <span class="font-mono text-xs text-muted-foreground">{APP_VERSION}</span>
          </div>
          <div class="flex items-center justify-between gap-3 px-3.5 py-3">
            <span class="text-sm text-foreground">Host version</span>
            <span class="text-xs text-muted-foreground">Not reported</span>
          </div>
          <div class="flex items-center gap-3 px-3.5 py-3">
            <Field title="Debug mode" subtitle="Verbose client logging for diagnostics." />
            <Toggle ariaLabel="Debug mode" checked={debug()} onChange={toggleDebug} />
          </div>
          <button
            type="button"
            onClick={() => copyText(diagnostics(), "diagnostics")}
            class="flex w-full items-center gap-3 px-3.5 py-3 text-left outline-none transition-colors hover:bg-muted focus-visible:bg-muted"
          >
            <div class="min-w-0 flex-1">
              <Field
                title="Copy diagnostics"
                subtitle="Version, connection, and settings — no secrets."
              />
            </div>
            <Copy class="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
          </button>
        </Card>
      </div>

      <Show when={confirmForget()}>
        <ConfirmForgetDialog onConfirm={forgetToken} onCancel={() => setConfirmForget(false)} />
      </Show>
    </div>
  );
}

/** Settings surface (desktop-shell): a right-docked inspector reachable from the
 * NavRail gear (a full-screen sheet on phone). Grouped calm-terminal sections —
 * Appearance, Connection & devices, Notifications, Device label & identity,
 * About & debug — mirroring the Android Settings screen, adapted honestly to the
 * browser WS client's actual capabilities. Mounted inside ChatView's row so the
 * desktop dock resolves against the frame, not the viewport. */
export function SettingsSheet() {
  return (
    <Show when={state.settingsOpen}>
      <SettingsPanel />
    </Show>
  );
}
