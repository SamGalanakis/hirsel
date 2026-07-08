/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** WebSocket URL of the Hirsel Host (or app/tools/mock-server.mjs in dev).
   * When unset: dev defaults to ws://<current-host>:8787 (the mock server);
   * production defaults to same-origin ws(s)://<origin>/ws, since the Hirsel
   * Host serves the built app itself. */
  readonly VITE_WS_URL?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
