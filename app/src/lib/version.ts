// Build identity shown in Settings → About. There is no server-provided host
// version over the WS protocol, so About reports the host as "Not reported"
// (parity with the Android client). `import.meta.env.MODE` is Vite's build mode
// ("development" when served by the dev server, "production" in a built bundle).
export const APP_VERSION = `web 0.0.0 · ${import.meta.env.MODE}`;
