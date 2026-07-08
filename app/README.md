# hirsel PWA

Mobile-first Vite + SolidJS + TypeScript client for hirsel (slice 1 - see
`/CONTEXT.md`, `/docs/SCOPE.md`, and `PROTOCOL.md` in this directory for the
wire protocol this implements verbatim). UI built on Tailwind 4 + Kobalte with
the shared component primitives and design tokens from the lashapp frontend.

## Develop against the mock host

No Rust host yet? `tools/mock-server.mjs` is a scripted stand-in that speaks
the full protocol from `PROTOCOL.md`, echoes messages back after ~1s, and runs
a small "delegate" scenario (send the message `delegate` to see it: agent
activity → chat reply → an Inbox Item with Quick Replies ~3s later → tapping a
Quick Reply gets acknowledged and archives the item).

```sh
npm install
npm run dev:mock   # mock WS server (port 8787) + vite dev server together
```

Open the printed local URL, enter `dev-token` at the first-run token prompt
(that's `MOCK_TOKEN` in `tools/mock-server.mjs`, override via env if you like).

## Develop against a real Hirsel Host

```sh
VITE_WS_URL=wss://your-host/ws npm run dev
```

`VITE_WS_URL` always wins when set. When unset, the default depends on mode:

- dev (`npm run dev`): `ws://<current-host>:8787` — the mock server's port.
- production build: same-origin `ws(s)://<origin>/ws` — the Hirsel Host
  serves `dist/` itself and exposes its WS endpoint at `/ws`, so a plain
  `npm run build` needs no configuration at all.

## Verification

```sh
npm run build   # tsc --noEmit && vite build
npm test        # vitest run - protocol reducer + reconnect backoff + component tests
npm run lint    # oxlint
```
