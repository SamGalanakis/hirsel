// Local-only client preferences and the small pure helpers the Settings
// sections share.
//
// The WS protocol (see PROTOCOL.md) carries only a token + replay cursor from
// this client — no device label, no roster, no pairing, no push. So these are
// honestly scoped to this browser: a cosmetic display label and a debug flag,
// both persisted locally and surfaced in the diagnostics blob, never sent to
// the Host.
import { copyWithToast } from "../../lib/clipboard";
import type { ConnectionStatus } from "../../store/types";

export const DEVICE_LABEL_KEY = "hirsel.deviceLabel";
export const DEBUG_KEY = "hirsel.debug";

export const PHASE_WORD: Record<ConnectionStatus, string> = {
  connecting: "connecting…",
  connected: "connected",
  reconnecting: "reconnecting…",
};

export function readLocal(key: string): string {
  try {
    return localStorage.getItem(key) ?? "";
  } catch {
    return "";
  }
}

/** A one-way, secret-free fingerprint of this browser's access token — safe to
 * show and copy (mirrors the Android client's iroh-identity fingerprint). */
export async function computeFingerprint(secret: string | null): Promise<string> {
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
export function maskToken(token: string | null): string {
  if (!token) return "none stored";
  if (token.length <= 6) return "•".repeat(token.length);
  return `${token.slice(0, 2)}${"•".repeat(Math.min(12, token.length - 4))}${token.slice(-2)}`;
}

export function copyText(value: string, label: string): void {
  void copyWithToast(value, `Copied ${label}`);
}

export function titleCase(s: string): string {
  return s.length > 0 ? s[0].toUpperCase() + s.slice(1) : s;
}
