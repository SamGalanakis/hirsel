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

Scripted words for the v1.4 surfaces (Processes tab + tool-call visibility):

- `delegate` — spawns a **sub-agent** process (agent+model chips) that runs with
  progress-summary updates then completes; watch it in the **Processes** tab move
  Running → Finished, and use **Ask to stop** while it runs.
- `tools` — a thinking turn that streams live tool-call rows under "Thinking…",
  then commits a reply carrying a **⚙ 2 tools** chip you can expand.
- `monitor` — creates a **monitor** process (code-style probe cmd) that "fires" a
  few seconds later, updating its summary.

Two processes (a running monitor and a finished sub-agent) are seeded at startup
so the tab is populated immediately.

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
