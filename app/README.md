# Hirsel web app

SolidJS reference client for Task Margins: one globally aware Hirsel, one standing composer, and Tasks as the only durable visible objects. Opening a task renders its constrained JSON interface and related conversation in place; Processes, Settings, and Canvas are temporary utilities.

## Development

The in-memory mock seeds three task shapes, conversation, and processes. It accepts any non-empty token by default; set `MOCK_TOKEN` only for an explicit rejection test.

```sh
npm install
npm run dev:mock
```

The browser uses same-origin `/ws` and `/blob` routes. Vite proxies them to the
local mock on port 8787, so only the Vite port needs to be exposed or forwarded.
Against a real local Host:

```sh
HIRSEL_TOKEN=dev HIRSEL_DEBUG=1 HIRSEL_PROVIDER=codex HIRSEL_IROH=0 \
  cargo run -p hirsel-host

HIRSEL_DEV_PROXY_TARGET=ws://127.0.0.1:3089 npm run dev
```

The Codex provider reads the existing OAuth session from `~/.codex/auth.json`.
In loopback debug mode, the browser may enter any non-empty token; production
continues to require the exact configured token.

`VITE_WS_URL=wss://your-host/ws` remains available for an explicit direct
remote endpoint, but is not needed for the normal forwarded development path.

The mock recognizes `delegate`, `tools`, and `monitor` messages for process/timeline development. Generated task actions are applied authoritatively and logged as `event_action` records. The seeded deploy Task demonstrates a data-driven multi-stage instrument: ship advances to a live canary checkpoint, promotion settles, and reopen restores that prior actionable stage without changing Task identity or conversation scope.

## Verification

```sh
npm run build
npm test
npm run lint
npm run e2e:task-margins
```

The browser runner boots isolated services, polls readiness, executes 38 objective desktop/phone/narrow gates (including complete outgoing frames, adaptive stages, utilities, focus, touch targets, and console/request failures), and writes [`../e2e/reports/task-margins-latest.md`](../e2e/reports/task-margins-latest.md). Detailed product flows live in [`../e2e/RULES.md`](../e2e/RULES.md) and its four current task-surface runbooks.
