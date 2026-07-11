import { createSignal } from "solid-js";

// Local-only client preferences with a reactive value + localStorage backing,
// so a Settings toggle and the effect that consumes it (e.g. App's title badge)
// stay in sync without threading state through the tree. Honestly scoped to this
// browser — never sent to the Host.
const TITLE_BADGE_KEY = "hirsel.titleBadge";

function readBool(key: string, fallback: boolean): boolean {
  try {
    const raw = localStorage.getItem(key);
    return raw === null ? fallback : raw === "1";
  } catch {
    return fallback;
  }
}

// Whether unread Pings show as "(3) hirsel" in the tab title (default on). A user
// who finds the badge distracting can silence it in Settings → Notifications.
const [titleBadgeEnabled, setTitleBadgeSignal] = createSignal(readBool(TITLE_BADGE_KEY, true));

export { titleBadgeEnabled };

export function setTitleBadgeEnabled(on: boolean): void {
  setTitleBadgeSignal(on);
  try {
    localStorage.setItem(TITLE_BADGE_KEY, on ? "1" : "0");
  } catch {
    /* best-effort; the toggle just won't persist across reloads */
  }
}
