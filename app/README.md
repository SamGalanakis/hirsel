# Hirsel web app

SolidJS client for Threads: durable subjects with their own messages, turns, activity, and generated instruments. One globally aware Hirsel receives messages addressed to the focused Thread. Processes, Settings, and Canvas remain temporary utilities.

## Development

The in-memory mock seeds an ordinary groceries Thread. It accepts any non-empty token by default; set `MOCK_TOKEN` only for an explicit rejection test.

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

The mock implements explicit root/child Thread creation, pinning, owned messages with explicit existing-artifact references, deterministic turns, explicit settlement/reopening, attachments, and same-token reconnect. It preserves client IDs for idempotent retries. `MOCK_SEED=none` starts with no Threads. Generated instrument continuation is verified against the real scripted Host.

## Verification

```sh
npm run build
npm test
npm run lint
node --test tools/mock-server.test.mjs
```

The real-host browser gate is `npm run e2e:threads`. It requires an explicit isolated `HIRSEL_THREAD_SMOKE_URL` and token; see [`../e2e/RULES.md`](../e2e/RULES.md) for setup, current-schema fixtures, adaptive instrument checks, and screenshots. The suite verifies desktop and phone layouts, creation, conversation/draft ownership, explicit lifecycle, and reconnect.

Artifacts are created explicitly by the Agent. The web client lists global artifacts
and combines a Thread’s artifact references with explicitly saved web links and Thread references in Related. Opening a preview keeps the
current Thread and its composer draft. Updates replace the current content under
the same artifact ID.

Solid artifacts use the pinned Solid 2.0 release candidate and default-export a JSX
component. Only `solid-js` and `@solidjs/web` imports are available. The locally
bundled compiler runs in a worker; generated code runs in an opaque iframe with
local interaction, no host credentials or Hirsel action bridge, and CSP that blocks
resource/network connections. HTML uses the same iframe boundary; files are UTF-8
text with download support. A fixed, source-checked message lets Escape inside an
interactive preview dismiss it; this UI-only signal grants no Hirsel actions.

Artifact verification uses isolated fixtures, never the live host:

```sh
npm run dev:artifact-preview
HIRSEL_ARTIFACT_TEST_URL=http://127.0.0.1:48594 npm run e2e:artifact-runtime
# A scripted host with a temporary data directory and the production web build:
HIRSEL_ARTIFACT_HOST_URL=http://127.0.0.1:PORT npm run e2e:artifacts
```

Conversation URLs use the CommonMark/GFM link renderer, including reference-style links. GitHub pull requests, issues and repository URLs and Linear issue URLs have local type icons and compact URL-derived labels; authored labels remain unchanged. Native anchors preserve browser modifiers and context menus, while an adjacent keyboard/touch menu offers Open in new tab, Copy link and explicit Add to Related. Recognition makes no metadata/favicon request and creates no artifact or saved association. Saved links support add/remove and authoritative history-scoped snapshots. Related and preview navigation keep the current Thread and draft.
