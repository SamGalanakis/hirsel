/// <reference types="vite/client" />
/// <reference types="vite-plugin-pwa/client" />

interface ImportMetaEnv {
  /** WebSocket URL of the Hirsel Host (or app/tools/mock-server.mjs in dev).
   * When unset, both development and production use same-origin
   * ws(s)://<origin>/ws. Vite proxies that path to HIRSEL_DEV_PROXY_TARGET in
   * development; the Hirsel Host serves it directly in production. */
  readonly VITE_WS_URL?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
