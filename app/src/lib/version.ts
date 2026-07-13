// Build identity of THIS client shown in Settings → About. The host reports its
// own version separately over `hello_ok` (see store `hostVersion`), rendered on
// the "Host version" row. `import.meta.env.MODE` is Vite's build mode
// ("development" when served by the dev server, "production" in a built bundle).
export const APP_VERSION = `web 0.0.0 · ${import.meta.env.MODE}`;
