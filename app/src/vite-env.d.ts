/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** WebSocket URL of the Hirsel Host (or app/tools/mock-server.mjs in dev).
   * Defaults to ws://<current-host>:8787 when unset. */
  readonly VITE_WS_URL?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
