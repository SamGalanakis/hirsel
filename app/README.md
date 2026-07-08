# hirsel PWA

Mobile-first Vite + React + TypeScript client for hirsel (slice 1 - see
`/CONTEXT.md`, `/docs/SCOPE.md`, and `PROTOCOL.md` in this directory for the
wire protocol this implements verbatim).

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

If `VITE_WS_URL` is unset it defaults to `ws://<current-host>:8787`, i.e. the
mock server's default.

## Verification

```sh
npm run build   # tsc -b && vite build
npm test        # vitest run - protocol reducer + reconnect backoff
npm run lint    # oxlint
```
