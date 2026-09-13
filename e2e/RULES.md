# Thread E2E rules

The real-model, agent-judged product scenarios live under [`../runbooks/`](../runbooks/)
and follow [`../runbooks/RULES.md`](../runbooks/RULES.md). Keep those semantic
browser checks separate from the deterministic scripted gates described here.

The deterministic gate is `just e2e`. It builds the production PWA and repository
Host, starts disposable services, and runs the Thread, artifact, showcase, blob
policy, and helper checks. All runners share `lib/harness.mjs` for browser
selection, free ports, Host lifecycle, polling, and WebSocket requests.

## Automated runs

Run from the repository root:

```bash
just e2e
```

The entrypoint launches a scripted/fake Host on a free loopback port. Its data
and neutral working directory are created below `$TMPDIR` and removed after the
run. The Host binary is derived from Cargo's target directory and the repository
root. `HIRSEL_E2E_PROFILE=release` selects the release binary; `debug` is the
default. Do not point an individual runner at an Owner's live host.

`CHROMIUM_EXECUTABLE` optionally selects an installed Chromium executable. When
unset, `just e2e` installs and Playwright discovers the browser revision pinned
by `app/package-lock.json`. No Playwright cache path or developer home directory
is encoded in the suite.

The top-level gate supplies service settings to its children. For focused runs,
build the PWA and Host first, launch an isolated scripted/fake Host, then provide
the runner-specific inputs:

```bash
HIRSEL_THREAD_SMOKE_URL=http://127.0.0.1:TESTPORT \
HIRSEL_THREAD_SMOKE_TOKEN=development-token \
npm run e2e:threads
```

`HIRSEL_THREAD_SMOKE_ARTIFACTS` selects the screenshot directory. Set
`HIRSEL_THREAD_SMOKE_ADAPTIVE=1` to include the generated-instrument continuation
and stale-revision checks. Artifact and showcase runners use
`HIRSEL_ARTIFACT_HOST_URL` and `HIRSEL_ARTIFACT_HOST_TOKEN`; an optional
`HIRSEL_APP_URL` may point the showcase browser at a separate loopback app. The
standalone artifact-runtime and SVG-preview runners use
`HIRSEL_ARTIFACT_TEST_URL`; SVG preview additionally requires
`HIRSEL_CAT_ARTIFACT_DB` and optionally accepts
`HIRSEL_SVG_ARTIFACT_SCREENSHOT`.

Evidence recovery uses `HIRSEL_ARTIFACT_HOST_URL`,
`HIRSEL_ARTIFACT_HOST_TOKEN`, `HIRSEL_ARTIFACT_DB`, and
`HIRSEL_ARTIFACT_EVIDENCE`; `HIRSEL_ARTIFACT_THREAD_ID` and
`HIRSEL_ARTIFACT_ID` default to `1` and `2`. The responsive fixture runner uses
`HIRSEL_RESPONSIVE_FIXTURE` and optionally `HIRSEL_RESPONSIVE_EVIDENCE`. The
Spaces/Tasks and real-model product runbooks optionally use
`HIRSEL_SPACES_EVIDENCE` and `HIRSEL_RUNBOOK_ARTIFACTS`. Every default evidence
path is below `$TMPDIR`.

The scripted suite creates test Threads and sends scripted messages. It makes no
provider calls. `HIRSEL_EXPECT_UNSAFE_INLINE=1` exists only for proving the
pre-fix blob behavior and must not be set for the normal gate.

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
