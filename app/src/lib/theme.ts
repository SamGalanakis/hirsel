import { createSignal } from "solid-js";

// The light/dark theme system. Three modes — System / Light / Dark — default
// System (follow the OS via `prefers-color-scheme`). Light/Dark force the
// scheme. The choice is persisted in localStorage (`hirsel.theme`) and applied
// by toggling `.dark` on <html>.
//
// First paint is handled by the tiny inline script in index.html (it reads the
// same key synchronously before the bundle loads, so there is no flash). This
// module owns the live behaviour: reflecting a user's pick immediately, and
// re-resolving when the OS scheme flips while in System mode. Keep the two in
// sync.

export type ThemeMode = "system" | "light" | "dark";

const STORAGE_KEY = "hirsel.theme";
// Browser-chrome tint: the dark canvas (oklch 0.141) ≈ #141414; the light
// canvas (oklch 0.985 0.002 286) ≈ #fafafb. Mirrors index.html.
const DARK_META = "#162126";
const LIGHT_META = "#f5faf7";

function readStored(): ThemeMode {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw === "light" || raw === "dark" || raw === "system") return raw;
  } catch {
    /* localStorage blocked — fall through to the default. */
  }
  return "system";
}

const darkQuery =
  typeof window !== "undefined" && typeof window.matchMedia === "function"
    ? window.matchMedia("(prefers-color-scheme: dark)")
    : null;

/** Whether the dark scheme should be active for `mode` right now. */
function resolveDark(mode: ThemeMode): boolean {
  if (mode === "dark") return true;
  if (mode === "light") return false;
  return darkQuery?.matches ?? false;
}

/** Toggle `.dark` on <html> and keep the theme-color meta in sync. The inline
 * head script does this first (pre-paint); this keeps it live after mount. */
function applyTheme(mode: ThemeMode): void {
  if (typeof document === "undefined") return;
  const dark = resolveDark(mode);
  document.documentElement.classList.toggle("dark", dark);
  document
    .querySelector('meta[name="theme-color"]')
    ?.setAttribute("content", dark ? DARK_META : LIGHT_META);
}

const [themeMode, setThemeModeSignal] = createSignal<ThemeMode>(readStored());

// Follow OS scheme changes while (and only while) the user is on System.
darkQuery?.addEventListener("change", () => {
  if (themeMode() === "system") applyTheme("system");
});

/** Persist + live-apply a theme choice. Called from Settings → Appearance. */
export function setThemeMode(mode: ThemeMode): void {
  setThemeModeSignal(mode);
  try {
    localStorage.setItem(STORAGE_KEY, mode);
  } catch {
    /* Persistence is best-effort; the live class toggle still applies. */
  }
  applyTheme(mode);
}

export { themeMode };
