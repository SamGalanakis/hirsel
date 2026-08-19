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

/** The HTTP origin that serves the same Host as a given WS URL: ws→http,
 * wss→https, and a trailing `/ws` path dropped (the Host serves the app, blob
 * bytes, the plugin REST surface, and plugin UI bundles from one origin root).
 * Returns a bare origin with no trailing slash, so callers concatenate a
 * host-relative path directly. Single source of truth for every out-of-band
 * fetch (blob assets in ws/client.ts, `/api/...` in lib/api.ts). */
export function httpBaseFromWs(wsUrl: string): string {
  try {
    const u = new URL(wsUrl);
    u.protocol = u.protocol === "wss:" ? "https:" : "http:";
    u.pathname = u.pathname.replace(/\/ws\/?$/, "");
    return u.toString().replace(/\/$/, "");
  } catch {
    return "";
  }
}

/** The HTTP origin for the configured endpoint. */
export function resolveHttpBase(): string {
  return httpBaseFromWs(resolveWsUrl());
}
