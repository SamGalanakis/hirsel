// The WS endpoint of the Hirsel Host, resolved in this order:
//  - Unset, "same-origin", or "/ws" → build ws(s)://<this-host>/ws, i.e.
//    connect back through whatever origin served the app. In dev this is the
//    Vite server, which proxies /ws → the configured Host or mock (see
//    vite.config.ts), so only the Vite port needs forwarding over a tunnel.
//  - any other VITE_WS_URL → used verbatim.
//
// Single source of truth for both the connection (App.tsx) and the honest
// read-only endpoint shown in Settings → Connection.
const sameOriginWs = () =>
  `${window.location.protocol === "https:" ? "wss://" : "ws://"}${window.location.host}/ws`;

export function resolveWsUrl(): string {
  const raw = import.meta.env.VITE_WS_URL;
  if (raw === "same-origin" || raw === "/ws") return sameOriginWs();
  return raw ?? sameOriginWs();
}
