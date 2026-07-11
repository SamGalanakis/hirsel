import { toast } from "./toast";

/** Copy `value` to the clipboard, resolving true on success. Uniformly handles
 * a missing Clipboard API (insecure context / old browser) and a rejected
 * permission — both resolve false rather than throwing. */
export async function copyToClipboard(value: string): Promise<boolean> {
  try {
    if (!navigator.clipboard?.writeText) return false;
    await navigator.clipboard.writeText(value);
    return true;
  } catch {
    return false;
  }
}

/** Copy `value` and toast the outcome — the shared "copy + confirm" used by the
 * message bubble copy action and Settings' copy rows (diagnostics, endpoint,
 * fingerprint). `successMessage` is the confirmation ("Copied message"); the
 * failure toast is a single sanctioned "Couldn't copy" error. Returns whether it
 * succeeded so callers can also flip local UI (e.g. the bubble's ✓ tick). */
export async function copyWithToast(value: string, successMessage: string): Promise<boolean> {
  const ok = await copyToClipboard(value);
  if (ok) toast(successMessage);
  else toast("Couldn't copy", { variant: "error" });
  return ok;
}
