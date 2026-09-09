# Thread E2E rules

The current product gate is `thread-smoke.mjs`: durable thread inventory,
thread-owned conversations and drafts, explicit settlement/reopening, reconnect,
and desktop/phone containment. Task Margins runners and their reports in this
directory are historical evidence for the superseded Event protocol. They are
not current-product gates and are no longer advertised npm commands.

## Automated runs

Build the PWA and launch an isolated Rust host with `HIRSEL_AGENT=scripted`,
`HIRSEL_DRIVER=fake`, `HIRSEL_IROH=0`, a temporary `HIRSEL_DATA_DIR`, and an
unused non-production port. Do not point this suite at an Owner's live host.

From `app/`:

```bash
HIRSEL_THREAD_SMOKE_URL=http://127.0.0.1:TESTPORT \
HIRSEL_THREAD_SMOKE_TOKEN=development-token \
npm run e2e:threads
```

`CHROMIUM_EXECUTABLE` selects an installed Chromium. Set
`HIRSEL_THREAD_SMOKE_EXPECT_ID=5` when using an isolated imported data snapshot to
verify the formerly invisible groceries thread. `HIRSEL_THREAD_SMOKE_ARTIFACTS`
selects the screenshot directory. Set `HIRSEL_THREAD_SMOKE_ADAPTIVE=1` to seed a scripted-only Thread instrument, exercise Continue in place, and reject replay of its stale revision without another message. The script creates test threads and sends
scripted messages; it makes no provider calls when the host is configured above.

For lightweight frontend work, `npm run dev:mock` serves the same Thread
commands with in-memory state, owned message history, idempotent message sends,
turns, and explicit lifecycle actions. Restart resets it. `MOCK_SEED=none`
starts with only the orchestrator, and each token owns an isolated world.
`node --test tools/mock-server.test.mjs` verifies its protocol boundaries.
Generated instrument continuation requires the real scripted host.

## Evidence and isolation

Drive the real app in a clean browser context. Use actual controls and
observable host frames; DOM injection and direct client-store mutation do not
count as proof. Seed host fixtures before boot and identify them in the report.
Use SQLite's backup API for a running source database, including WAL contents.
The live data directory and live host process must remain untouched.

Record checkout revision/dirty state, exact invocation, service mode, browser
viewports, screenshots, incoming/outgoing frames, and failures. Poll observable
conditions with deadlines. Stop on service errors, malformed frames, unexpected
browser errors, failed requests, timeouts, or horizontal overflow. Fixes and
fresh proof runs are separate operations. Clean up only the processes started
by the isolated run.
