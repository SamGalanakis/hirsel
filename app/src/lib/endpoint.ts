// The WS endpoint of the Hirsel Host, resolved in this order:
//  - VITE_WS_URL="same-origin" (or "/ws") → build ws(s)://<this-host>/ws, i.e.
//    connect back through whatever origin served the app. In dev this is the
//    vite server, which proxies /ws → the Host (see vite.config.ts), so only
//    the vite port needs forwarding over a tunnel.
//  - any other VITE_WS_URL → used verbatim.
//  - unset: in dev default to the mock server's port; in production the Host
//    serves this app from the same origin with its WS at /ws.
//
// Single source of truth for both the connection (App.tsx) and the honest
// read-only endpoint shown in Settings → Connection.
const sameOriginWs = () =>
  `${window.location.protocol === "https:" ? "wss://" : "ws://"}${window.location.host}/ws`;

export function resolveWsUrl(): string {
  const raw = import.meta.env.VITE_WS_URL;
  if (raw === "same-origin" || raw === "/ws") return sameOriginWs();
  return raw ?? (import.meta.env.DEV ? `ws://${window.location.hostname}:8787` : sameOriginWs());
}
