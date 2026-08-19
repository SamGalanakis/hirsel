// Settings → Connection & devices: the live connection status, this browser's
// row, the build-time endpoint and the stored access token (with its
// forget affordance). Pairing and the device roster live on the Host — the web
// client holds only this browser's token, so there is nothing else to list.
import { onMount, type JSX } from "solid-js";
import { createFocusTrap } from "../../lib/focus";
import { state } from "../../store/store";
import { getStoredToken } from "../../ws/client";
import { ConnectionPill } from "../ConnectionPill";
import { Button } from "../ui/button";
import { maskToken, PHASE_WORD } from "./prefs";
import { Group, CopyRow, Field, SectionHeader } from "./rows";

export function ConnectionSection(props: {
  endpoint: string;
  deviceLabel: string;
  onForget: () => void;
}): JSX.Element {
  return (
    <>
      <SectionHeader>Connection &amp; devices</SectionHeader>
      <Group class="divide-y divide-border">
        <div class="flex items-center justify-between gap-3 py-3">
          <span class="text-sm text-foreground">Status</span>
          <ConnectionPill />
        </div>
        <div class="flex items-center gap-3 py-3">
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
              {props.deviceLabel || "This browser"}
            </div>
            <div class="mt-0.5 text-xs text-muted-foreground">
              This device · {PHASE_WORD[state.connection]}
            </div>
          </div>
          <span class="shrink-0 rounded-full bg-muted px-1.5 py-0.5 text-meta font-medium text-muted-foreground">
            You
          </span>
        </div>
        <div class="py-3">
          <Field title="Server endpoint" subtitle="Where this client connects. Set at build time." />
          <div class="mt-2">
            <CopyRow value={props.endpoint} label="server endpoint" mono />
          </div>
        </div>
        <div class="flex items-center gap-3 py-3">
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
            onClick={props.onForget}
          >
            Forget
          </Button>
        </div>
      </Group>
      <p class="mt-2 text-xs leading-snug text-muted-foreground">
        Pairing and the device roster live on the Host — the web client holds only this browser's
        token, so it can't list or unpair other devices.
      </p>
    </>
  );
}

export function ConfirmForgetDialog(props: { onConfirm: () => void; onCancel: () => void }) {
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
        ref={(node) => { dialogRef = node; }}
        tabindex={-1}
        role="alertdialog"
        aria-label="Forget token"
        class="w-full max-w-[320px] rounded-xl border border-border bg-card p-4 shadow-lg outline-none"
      >
        <h3 class="m-0 text-sm font-semibold text-foreground">Forget this token?</h3>
        <p class="mt-1.5 mb-4 text-sm leading-relaxed text-muted-foreground">
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
