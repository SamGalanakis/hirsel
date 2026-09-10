# C27 — Test infrastructure and cross-system fixtures

## Findings

### F1 — Mock idempotent `create_thread` replay can regress a Thread summary

**Verdict: recommend. Confidence: high.** The mock stores protocol-derived status fields on the mutable Thread record and separately recomputes them in `summary`. Its cached idempotency response retains the raw record, so a retry of the same create request after a message/turn can return `running_turn: null`, `queued_turn_count: 0`, `last_finished_turn: null`, and the old `last_activity_at` at the same revision as the newer summary. The web store accepts equal revisions, so the stale replay can overwrite the status used by the navigation and status UI.

**Exact evidence and affected layers.** There is no DDL layer in this fixture; the affected layers are the mock's in-memory record, its Thread wire projection, and the web consumer:

```text
app/tools/mock-server.mjs:21-23
function makeThread(id, title, parent_thread_id = null) {
  return { id, title, icon: null, showcased_artifact_id: null, parent_thread_id, pinned_at: null, description: "", instrument: null, attention: "quiet", settled_at: null, archived_at: null, snoozed_until: null, read: false, created_at: now(), updated_at: now(), revision: 1, running_turn: null, queued_turn_count: 0, last_finished_turn: null, last_activity_at: now() };
}

app/tools/mock-server.mjs:32-36
function summary(world, thread) {
  const turns = world.turns.filter(turn => turn.thread_id === thread.id);
  const terminal = turns.filter(turn => turn.finished_at).sort((a, b) => Date.parse(b.finished_at) - Date.parse(a.finished_at) || b.id - a.id);
  const times = [thread.created_at, ...world.messages.filter(row => row.thread_id === thread.id).map(row => row.ts), ...world.activities.filter(row => row.thread_id === thread.id).map(row => row.ts), ...turns.flatMap(turn => [turn.started_at, turn.finished_at]).filter(Boolean)];
  return { ...thread, running_turn: turns.find(turn => turn.state === "running") ?? null, queued_turn_count: turns.filter(turn => turn.state === "queued").length, last_finished_turn: terminal[0] ?? null, last_activity_at: new Date(Math.max(...times.map(Date.parse))).toISOString() };
}

app/tools/mock-server.mjs:129-135
      const prior = world.requests.get(frame.client_id);
      if (prior) { if (prior.type !== "thread_created" || prior.thread.title !== frame.title.trim() || prior.thread.parent_thread_id !== frame.parent_thread_id) throw new Error("client_id already used"); send(ws, prior); return; }
      const thread = makeThread(world.nextThread++, frame.title.trim(), frame.parent_thread_id);
      world.threads.push(thread);
      const result = { type: "thread_created", client_id: frame.client_id, thread };
      world.requests.set(frame.client_id, result);
      broadcast(world, { type: "thread_upsert", thread });
      send(ws, result);

app/tools/mock-server.mjs:53-58 and :60-75
  const turn = { id: world.nextTurn++, thread_id: message.thread_id, owner_message_id: message.id, requester_thread_id: threadFor(world, message.thread_id).parent_thread_id, requester_turn_id: null, agent_message_id: null, state: "queued", started_at: now(), finished_at: null };
  world.turns.push(turn);
  world.queue.push({ turn, message });
  broadcast(world, { type: "thread_turn", turn });
  runNext(world);
...
  turn.state = "running";
  broadcast(world, { type: "thread_turn", turn });
...
    Object.assign(turn, { state: "completed", agent_message_id: reply.id, finished_at: now() });

app/src/threads/model.ts:13-16
  const prior = threads.find(t => t.id === incoming.id);
  if (prior && prior.revision > incoming.revision) return threads;
  return [...threads.filter(t => t.id !== incoming.id), incoming].sort((a, b) => b.id - a.id);

app/src/threads/store.ts:199-205
    case "thread_upsert":
    case "thread_created":
      setThreadState(draft => { reconcile(upsertThread(threadState.threads, message.thread), "id")(draft["threads"]); });
      if (message.type === "thread_created") {
        const pending = requests.get(message.client_id);
        if (pending) { clearTimeout(pending.timer); requests.delete(message.client_id); pending.resolve(message.thread); }
      }
```

The authoritative wire type intentionally exposes the four derived fields (`app/src/threads/types.ts:19-22`; `crates/hirsel-proto/src/thread.rs:35-43`). The web interprets them in `app/src/threads/status.ts:11-25`, `app/src/threads/ThreadNavigation.tsx:108`, and `app/src/threads/actions.ts:34`.

**Reachable duplicate truth.** A client can send `create_thread(client_id="c")`, then `send_thread_message` to the returned thread, then retry the same create request after losing the first response. `world.turns` and `world.messages` now make `summary(world, thread)` report a running/completed turn and a newer activity timestamp, but no path updates the four copies on `thread` itself. The retry sends the cached raw object from `world.requests`; its revision is unchanged, and `upsertThread` accepts the equal revision. This is a reachable write/replay path, not a live-data claim. Existing tests demonstrate only the pre-turn duplicate (`app/tools/mock-server.test.mjs:38-47`, `app/src/mock-server.contract.test.ts:127-135`); no fixture retries create after a turn or asserts that status cannot regress.

**Consumer query.** Reproducible query:

```sh
rg -n --glob '!node_modules/**' --glob '!target/**' \
  'running_turn|queued_turn_count|last_finished_turn|last_activity_at' \
  app/tools/mock-server.mjs app/tools/mock-server.test.mjs \
  app/src/mock-server.contract.test.ts app/src/threads
```

Snapshot result: **23 matches**, including the mock's two competing definitions, test fixtures, and the web's status/navigation/action consumers. The blast radius is the mock dev/contract path and every UI surface consuming Thread status; the Rust host's projection is consumer context, not owned here.

**Target representation.** Keep a mock-only base `ThreadRecord` with durable fields only (`id`, title/parent, lifecycle metadata, timestamps, revision, etc.). Keep `running_turn`, `queued_turn_count`, `last_finished_turn`, and `last_activity_at` only in `summary(world, record)` as the wire `ThreadSummary`. Store an idempotency receipt as request fingerprint plus thread ID (or regenerate the cached response from the current record) and construct `thread_created` from `summary` on both first response and replay. The web/Rust wire `Thread` remains unchanged; only the fixture's internal ownership and conversion change, removing the stale second copy.

**Smallest credible scope.** `app/tools/mock-server.mjs`; add the post-turn retry regression to `app/tools/mock-server.test.mjs` and/or `app/src/mock-server.contract.test.ts`. Existing artifact idempotency already regenerates `artifactSummary` on replay (`app/tools/mock-server.mjs:90-94`), which is the local shape to follow.

**Risk and validation.** Low-to-medium cutover risk: initial create responses may normalize `last_activity_at` to the summary's creation timestamp, and equality assertions should check protocol semantics rather than incidental object identity. Add a test that retries create while a turn is running and after completion, then verifies the returned summary and reconnect inventory retain the current turn/activity fields; run the existing mock contract tests and frontend type/lint gates after implementation. No tests or application code were run in this audit.

### F2 — Retired task-era harness and external-model runbook are unowned dead surface

**Verdict: recommend. Confidence: high.** `e2e/lib/harness.mjs` is a 181-line shared API whose only exported concepts are task-era port names and task selectors, while no current repository file imports or calls it. `e2e/external-model-smoke/runbook.md` explicitly calls itself historical/retired and still points to deleted files, commands, and report paths. Keeping both leaves a false test-infrastructure surface after the Thread cutover.

**Exact evidence.**

```text
e2e/lib/harness.mjs:7-12
export const PORTS = Object.freeze({
  taskMargins: Object.freeze({ mock: 39127, vite: 39128 }),
  taskHost: Object.freeze({ host: 39129, vite: 39130 }),
  externalSmoke: Object.freeze({ host: 39131 }),
  responsiveSweep: Object.freeze({ mock: 39132, vite: 39133 }),
});

e2e/lib/harness.mjs:52-55
export async function appReady(page) {
  await page.locator('[data-slot="composer-shell"]').waitFor();
  await page.locator('[data-slot="task-index"] [data-task-id]').first().waitFor();
}

e2e/external-model-smoke/runbook.md:1-6
> Historical Event/Task protocol runbook. For the current Thread product gate, see [E2E rules](../RULES.md). The old npm runner commands below are retired.
...
> [`../generated-task-ui/runbook.md`](../generated-task-ui/runbook.md) remains the required gate.

e2e/external-model-smoke/runbook.md:24-32 and :39-42
npm run e2e:task-host-external-smoke
...
HIRSEL_EXTERNAL_SMOKE=1 HIRSEL_SMOKE_PROVIDER=codex npm run e2e:task-host-external-smoke -- --run
...
[`../reports/task-host-external-smoke-latest.md`](../reports/task-host-external-smoke-latest.md)
and its JSON twin.
```

The current package exposes only the Thread/artifact gates (`app/package.json:15-18`), and the current runbook names `thread-smoke.mjs`, the mock server, and `mock-server.test.mjs` as the active paths (`e2e/RULES.md:3-30`). `e2e/generated-task-ui/runbook.md`, `e2e/task-host-external-smoke.mjs`, and both referenced report files are absent at this snapshot.

**Consumer query and reachability.**

```sh
rg -n --glob '!node_modules/**' --glob '!target/**' \
  --glob '!e2e/lib/harness.mjs' \
  'harness\\.mjs|lib/harness|PORTS|appReady\\(' . --hidden
# 0 matches

rg -n --glob '!node_modules/**' --glob '!target/**' \
  --glob '!e2e/external-model-smoke/runbook.md' \
  'task-host-external-smoke|generated-task-ui|task-margins-runner|task-responsive-keyboard|scrollback-runner|task-host-runner' . --hidden
# 0 matches
```

There is no reachable write path, runtime consumer, existing test, or fixture for this surface in the repository. This is an ownership/dead-code finding rather than an invalid state or duplicate-truth finding; no database, wire, or conversion layer is affected.

**Target representation and scope.** Delete `e2e/lib/harness.mjs` and the explicitly historical `e2e/external-model-smoke/runbook.md`. Keep the current Thread rules and package scripts. If an external-model smoke is still desired, recreate it as a current Thread-owned runbook and runner in a separately reviewed change; do not preserve the removed Task contract under a live-looking path.

**Risk and validation.** Local regression risk is low because the source-local consumer query is empty and the runbook admits retirement; the remaining risk is an undocumented external CI/operator invocation outside this checkout. Before cutover, check CI manifests and any external automation for those exact paths, then run documentation/link checks and verify the current Thread/artifact commands remain present. No tests or application code were run in this audit.

## Coverage contract and explicit skips

All exact whole-file owners in the dispatch were inspected. No shared exact definitions were assigned to this cluster. The following inventory records the ownership boundary, consumer context, and disposition:

| Owned file | Exact definitions/behavior reviewed | Consumer/conversion context | Status |
| --- | --- | --- | --- |
| `app/tools/artifact-smoke.html:1` | CSP, root, module entry | `e2e/artifact-runtime-smoke.mjs:8`; `app/artifact-preview.config.ts:4` | Skip: intentional fixture entrypoint |
| `app/tools/artifact-smoke.tsx:1-6` | Solid signal, `window.replaceArtifact`, `ArtifactPreview` fixture | Runtime browser smoke and artifact preview build | Skip: intentionally minimal editable artifact fixture |
| `app/tools/mock-server.mjs:1-315` | Auth/world state, Thread/message/turn/artifact/blob records, summaries, idempotency, websocket/HTTP handlers | `app/package.json:9-10`; `app/tools/mock-server.test.mjs:22`; `app/src/mock-server.contract.test.ts:104`; web Thread types/store/status; Rust protocol read for contract comparison | F1 recommend; lower leads skipped below |
| `app/tools/mock-server.test.mjs:1-68` | websocket client frame queue and reconnect/lifecycle contract test | Spawns the owned mock server; `e2e/RULES.md:29` documents it | Skip as a finding: current coverage is real and bounded, but lacks the F1 post-turn replay case |
| `app/vitest.setup.ts:1-62` | Solid event flush, plugin/network latch, jsdom browser API stubs, cleanup | `app/vite.config.ts:118` setupFiles | Skip: deliberate shared test-environment ownership; no invalid state or duplicate truth found |
| `e2e/RULES.md:1-45` | Current Thread gate, isolated-host/auth/evidence rules, mock gate | `app/package.json:15-18`; all current smoke scripts | Skip as a file finding; provides current-owner evidence for F2 |
| `e2e/artifact-runtime-smoke.mjs:1-49` | isolated iframe, reload/edit/offline/navigation assertions and browser lifecycle | `app/package.json:18`; `app/tools/artifact-smoke.html` | Skip: assertions and resource boundaries are locally coherent |
| `e2e/artifact-thread-smoke.mjs:1-60` | websocket request helper, artifact publication, two-thread sharing, viewport/focus/reload checks | `app/package.json:17`; host debug publication endpoint | Skip: no material representation/control-flow defect found |
| `e2e/external-model-smoke/runbook.md:1-42` | historical Task contract, retired commands and report paths | No current script/file consumer; deleted targets confirmed absent | F2 recommend |
| `e2e/lib/harness.mjs:1-181` | port map, process/poll/teardown, old host build/report helpers, auth probe | No current repository consumer | F2 recommend |
| `e2e/thread-protocol/runbook.md:1-18` | current Thread protocol validation and operator boundaries | Current host/frontend/Rust gates, read-only runbook | Skip: documentation only; no stale owned implementation |
| `e2e/thread-showcase-smoke.mjs:1-69` | showcase publish/replace/remove, preview/download, desktop/phone checks | Manual artifact host smoke; related artifact code and Thread protocol | Skip: no material defect; absence of a package script is not enough to infer deadness |
| `e2e/thread-smoke.mjs:1-130` | current primary gate, desktop/phone Thread lifecycle, drafts, frames, adaptive stale revision | `app/package.json:15`; `e2e/RULES.md:3-23` | Skip: current documented gate; no material simplification found |

### Lower leads explicitly skipped

- The mock's `thread_action` table accepts `snooze` as `frame.data?.until` without validation (`app/tools/mock-server.mjs:241-242`), while the authoritative host requires exactly one future RFC3339 `until` (`crates/hirsel-host/src/lib.rs:583-603`). The UI sender always emits the valid shape (`app/src/threads/actions.ts:22-24`), and the owned mock tests contain zero `snooze`/`unsnooze` actions. This is a real fixture-contract test gap, but it is a narrower validation concern at the adjacent protocol boundary and is not promoted alongside the two higher-blast-radius findings.
- Browser-helper duplication across the three artifact/showcase scripts is a small local line-saving opportunity, not a materially simpler ownership or state representation; it is skipped under the audit rules.
- Blob-ticket lifetime/global map, fixed browser executable fallbacks, and polling details were reviewed as operational concerns. No source-local evidence established a material representation or ownership defect within this cluster.
- `PRODUCT` orthogonal Thread dimensions and the Rust host's canonical summary/storage are consumer context only; no C27 recommendation changes those adjacent owners.

## Cross-cutting and deduplication pass

No cross-cutting pattern was promoted. F1 is assigned only to C27 because the defect is in the mock's cached replay/projection ownership; the Rust protocol summary is referenced only as consumer/conversion context. F2 is assigned only to C27 because the dead files are owned test tooling/docs. The exclusions and settled outcomes were checked; neither finding duplicates #2–14 or the coordinator's listed F02–F14 outcomes.

## Audit log and final verification

- Read the assigned specification, `/tmp/hirsel-combined-audit/exclusions.md`, repository `CONTRIBUTING.md` and `CLAUDE.md`, and both required audit skills.
- Verified before inspection: `HEAD=3ee0621a603659ab0168f565b99012b642415419`, `HEAD^{tree}=a4aac830c45398a66591f2c44b707aaf3cef281b`, and empty source `git status --porcelain`.
- Inspected every owned file with bounded `nl`/`sed`; searched exact symbols and consumers with `rg`; read the relevant web Thread type/store/status, current package/config consumers, and authoritative Rust validation/projection only as consumer context. Used `git show`/`git blame` only for the retired harness history.
- Performed independent coverage, ownership/duplication, schema/state, materiality, and priority passes. No source edits, tests, builds, installs, migrations, provider calls, live-data/config reads, commits, or process/session actions were performed.
- Final source verification: repeat `git rev-parse HEAD HEAD^{tree}`, `git status --porcelain`, and a path-bounded diff check for all 13 owned source files. Expected HEAD/tree remain unchanged and the source diff is empty. This report is the only deliverable written, at `/tmp/hirsel-combined-audit/workers/C27-TEST-INFRASTRUCTURE.md`.

Fix F1 first: it can make the mock silently erase current Thread status on an ordinary idempotent retry; F2 is an independent low-risk cleanup after that regression shape is captured.
