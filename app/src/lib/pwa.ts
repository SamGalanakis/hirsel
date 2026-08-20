import { registerSW } from "virtual:pwa-register";

/** Service-worker registration for the installed app.
 *
 * Why this file exists at all: `registerType: "autoUpdate"` alone does NOT
 * auto-update. When no module imports `virtual:pwa-register`, vite-plugin-pwa's
 * `injectRegister: "auto"` falls back to emitting a bare `registerSW.js`
 * (`navigator.serviceWorker.register("/sw.js")`) with no lifecycle listeners.
 * A new deploy then installs and (via `skipWaiting`/`clientsClaim`) takes
 * control *behind* the running page — but that page keeps the shell and chunks
 * it already loaded, so the reload that triggered the update still shows the
 * old build. Only a second reload picks it up, and an installed standalone
 * window that is never reloaded never updates at all, which is why the SW had
 * to be unregistered by hand.
 *
 * Importing the virtual module instead installs the autoUpdate path's
 * `activated` listener, which reloads once the new worker takes over: one
 * ordinary reload lands on the new build. Composer drafts are persisted to
 * localStorage per surface, so that reload does not cost typed text.
 */
export function registerServiceWorker(): void {
  if (!("serviceWorker" in navigator)) return;
  registerSW({
    immediate: true,
    onRegisteredSW(_swUrl, registration) {
      if (!registration) return;
      // A standalone window can go days without a navigation, and a navigation
      // is otherwise the only moment the browser re-fetches sw.js. Re-check
      // whenever the app comes back to the foreground — the point where the
      // user is least likely to be mid-thought.
      document.addEventListener("visibilitychange", () => {
        if (document.visibilityState === "visible") void registration.update().catch(() => {});
      });
    },
  });
}
