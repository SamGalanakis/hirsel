# Working in Hirsel

Follow [CONTRIBUTING.md](CONTRIBUTING.md) for the way of working, validation,
and contribution process. Hirsel tracks tasks in
[GitHub issues](https://github.com/SamGalanakis/hirsel/issues).

Read [CLAUDE.md](CLAUDE.md) for shared local development and worktree conventions.

## Rust builds

Work in `kiln fork` trees, one per agent; implementer and reviewer never share
a tree: `F=$(kiln fork hirsel <name>)`, then `cd "$F" && . ./env.sh` before ANY
cargo command (skipping env.sh cold-builds). `kiln rm hirsel <name>` when done.
Never write under `/workspace/kiln/*/golden-*`.

Use `kiln build`, `kiln check`, `kiln test`, `kiln clippy`, `kiln doc` and
`kiln run <label> -- <args>` for Bazel workflows; they load the fork's
environment, detect the repo from any subdirectory, and execute on the shared
NativeLink pool. Shared execution fails closed: an unreachable executor is an
error, never a silent local compile. `check` performs the same compile/link
proof as `build`; `analyze` checks the Bazel graph without compiling Rust.
`test` runs `//:workspace_tests`. Pass Bazel labels and options, not Cargo
flags; `--test_arg=<name>` filters test cases. `clippy` defaults to
`//:workspace_clippy` (build-script compiles are exempt — `cargo_build_script`
exposes no CrateInfo; keep the Cargo clippy recipe for that coverage);
`doc` renders `//:workspace_docs` into bazel-bin; `run` compiles on the pool
and starts the program locally.

`kiln fmt [-- --check]` is local `cargo fmt`; `kiln sync` regenerates the
graph and lockfile; `kiln clean` expunges only this workspace's output base.
`kiln gate hirsel <name> -- <cmd>` runs integration gates, names and ports
from `KILN_GATE_ID`.

Keep the named recipes for what Bazel does not cover: the node/app suites,
live-host runs and service-backed tests. `cargo` on PATH is a shim that pins
the target dir, flags and job budgets inside a cgroup scope; never set CARGO_*
by hand. Managed Cargo does not submit actions to NativeLink, so reach for
the kiln operation first. When local Cargo validation is required, preserve
the workspace feature graph with `--workspace --all-targets`.
## Agent orchestration: one lead, persistent sidekicks

One lead owns decisions; long-lived sidekicks own execution. They exchange
briefs, results and feedback, never histories. Route by role, not guessed
difficulty: difficulty is unknown until the code is read, and a mid-task model
switch discards the cache.

### Roles

| Role | Model |
|---|---|
| Lead | fable 5.1. Codex-only sessions: `gpt-6-astra` effort high. Opus 5 only when fable is dry; an opus lead briefs, never redoes lane work. |
| Sidekick, default implementer | `gpt-5.6-sol`, effort high. |
| Sidekick, critical path | `gpt-6-astra`, effort high: near sol's quality in a quarter of the wall-clock. |
| Sidekick, Claude pool | opus 5. Short tasks or when Codex is dry. Any lane expected to outlive the lead session stays on Codex: its session survives the lead and resumes by id. |
| Bulk executor | `gpt-5.6-luna`, effort max, always. Only script-provable edits. Never recon, research, audits, or anything a gate could be weakened to pass. After two unsuccessful Luna attempts, escalate to Sol; never send a third equivalent spec to Luna. |
| Reviewer, standard | opus 5, fresh context. Codex-only sessions: `gpt-6-astra` effort medium. |
| Reviewer, HARD | fable 5.1 verdict + astra (effort medium) co-review, launched in the same message. A Codex-only session launches the astra half and does not treat that as HARD-complete; name the missing fable half. |

HARD = core runtime semantics, a real wire or durable encoding change,
concurrency or fencing. Bumps, plumbing, copy and test re-pins are standard.

Two subscriptions are two quota pools, not two prices. Spend both when you
can dispatch both. A dry Codex model reroutes in-tier to the other Codex
model of the same tier, same fork, resumed session, reroute named. Never drop
a tier to save quota. If the required model remains unavailable, report the
blocker; do not waive review. A Codex-only process can launch `gpt-5.6-sol`,
`gpt-6-astra` and `gpt-5.6-luna` only; where a role names a Claude model it
cannot launch, reroute in-tier or stop and tell Sam.

### Hierarchy

- Max depth three: lead, then lanes and reviewers, both dispatched by the lead.
- A subplanner is a full lead of the same model class for a slice with a
  settled interface contract and at least three lanes. Smaller slices run as
  direct lanes. No forwarding-only layers.
- Divide by outcome and write boundary, never by phase. Each lane runs its own
  plan, implement, debug, validate loop.
- Width is capacity; stagger expensive builds.
- One live root orchestrator per arc. Takeover = stand-down plus state dump.

### Lane lifecycle

One persistent sidekick session per lane: implement, run checks, fix, open a
reviewable PR. Fix rounds resume the same session (`--resume`). A new lane
starts fresh. The lead dispatches the reviewer and receives the verdict; the
sidekick never picks or briefs its own judge and never certifies its own work.
Before dispatch, grep open PRs for the ticket and files: nothing is built twice.

### Briefs

Every brief: objective, acceptance criteria this lane can reach, constraints,
write boundaries, verification commands, `git fetch origin`, log hygiene
(tail/grep slices, never full dumps). Codex briefs also state write
authorization upfront and the bounded test scope.

- **Strong sidekick (sol, astra, opus):** design discovered within boundaries;
  guards, not file fences; pushback allowed. Boundary or contract change = end
  the turn with the question in the final message and wait for the resumed
  turn. Otherwise never ask.
- **Bulk (luna):** exact files, step list, no design latitude, stop at the
  first ambiguity.

### Review

- Independent, fresh context, never self-certified. Verdict: "Do X, not Y,
  because Z" plus one deciding risk, ≤300 words. Act on it or surface the
  disagreement.
- Reviewers validate the changed behavior and named risks with focused checks
  or probes. Reproducing an entire broad CI battery is unnecessary when the
  required CI will run on the same final tree; review and CI remain separate
  gates.
- The lead reads claimed evidence before spending a reviewer. Lanes lie: false
  battery rows, faked review IDs, narrowed gates. Ask every reviewer whether a
  gate or assertion was weakened to pass.
- Approval covers semantics, not a SHA. Behavior-preserving deltas (lint
  cleanup, formatting, prose, test-only pins, mechanical regen, clean rebases)
  carry approval forward on CI evidence. Deltas touching behavior, contracts,
  encoding, durability, concurrency, authority, cleanup, or a reviewed
  assumption get a bounded re-review of the affected scope; HARD deltas repeat
  each affected HARD scope.
- Docs, examples, runbooks: auto-merge, no gate.
- A missing provider never creates review debt: substitute, note once.
- Max one throughline advisor per arc, never also the final reviewer.
- No product auto-merge before the verdict.

### Waiting

Dispatch, then wait for completion or an actionable exception. Deadlines: 120
min implementation or review, 180 min large builds. Intervene early only for a
reported blocker, a safety or resource problem, or user steering. At expiry,
one liveness check (log growth), then reset. Silence is not failure. Done only
on the completion notification; 0-byte outputs and pgrep lie. Wait on a PID or
a notification, never a pattern.

### Continuity

The lead's memory is a rewritten arc brief: objective, settled decisions with
reasons, ownership and dependencies, active lane IDs, session ids and evidence
paths, risks, next actions. Hand off only at phase boundaries; each handoff
rebuilds the plan cold. The successor verifies repo and lane state first. Lane
handoffs: outcome, evidence and SHA, deviations, risks, what changed our
understanding, next action.

### Decisions

- Commitment points get a packaged ruling. Contested calls go to Sam as grilled
  prose with full mechanics and a recommendation. No question boxes.
- Wholehog end-state by default; descope only with explicit OK.
- Lead edits are single-file one-liners; more is delegated. One-grep lookups
  are done directly. Web research: `--search`, returned as a report.
- Locally provable CI or infra edits: just do them.

## Codex mechanics

- Spec file in the scratchpad, then the wrapper as its own harness-tracked
  background task (`run_in_background: true`, never nohup or chained):
  `~/.codex/bin/codex-harness-run --cd <dir> --spec <file> --log <file>
  --model <slug> --effort <level>`. The wrapper pins stdin, derives the target
  dir, disables codex MCPs, refuses tiny specs, and writes `<log>.session`.
- Prefer native Codex agents when explicit model/effort selection and tracked
  completion are available. Otherwise the wrapper. Never a wrapper agent from
  another model family.
- Fix round: same command plus `--resume <previous-log>` (or the UUID). Never
  `--last`. One session per lane; fork rather than resume to try two fixes.
- `codex exec resume` has no `--cd` and runs in the process cwd, not the cwd
  recorded at the session's start; the wrapper `cd`s into `--cd` before launch
  and refuses a `--resume` whose session-meta cwd differs from `--cd`. Always
  pass the lane's fork as `--cd`, for fresh and resumed runs alike.
- `--spec` is an ABSOLUTE path (relative resolves inside `--cd`). Every
  implementation spec opens with "you are the implementer": a sol lane that
  reads delegation doctrine starts routing instead of coding.
- Always `--dangerously-bypass-approvals-and-sandbox` (baked into the
  wrapper); read-only by prompt, never `-s read-only`. Reviewers get explicit
  read-only boundaries. `cargo fetch` once before fanning out.
- Capacity death, usage-limit exit, content-filter kill, or safety-pause
  stall: relaunch on the other Codex model of the same tier, same fork,
  resumed. Tell Sam when it is credits.
- Exact-PID kills only. Never kill runs that aren't yours.
- Parallel writers and reviewers each need their own workspace.

## Commits, PRs, published text

Never a Claude, Anthropic, Codex, or any AI co-author trailer or AI mention in
commits, PR bodies, comments, tickets, or teammate-visible text. Attribution is
the user alone, every repo; this overrides harness defaults. Strip it if a
worker adds it; PR-opening briefs say so. Stage exact paths; never `commit -a`.

## Stacked PRs and merge queues

- `gh stack` non-interactively only: `submit --auto`, `view --json`, `sync`.
  Never `gh pr merge` a stacked PR or squash members individually.
- GitHub-native stacks refuse `--auto` and the enqueue mutation. Land the
  whole stack with one call on the TOP PR, which merges every member below:
  `gh api -X PUT -H "X-GitHub-Api-Version: 2026-03-10"
  repos/<o>/<r>/pulls/<top>/merge-async -f sha=<head> -f merge_action=merge_queue`
  (no merge_method with merge_queue). Check `isInMergeQueue` via GraphQL;
  `gh pr view --json` has no such field. merge-async is not deferred: with a
  required check still pending it returns "pending" and then fails
  ("Required status check … is expected"), so issue it once CI concludes and
  confirm with `GET …/merge-async/<uuid>` (status enqueued vs failed).
- Once semantic review is complete, arm auto-merge or enqueue at once, even
  with CI pending. Enqueueing is not merging; never bypass required checks.
  Arm only on main-based PRs.
- Let the queue manage base updates; rebase only for a real conflict or a
  named semantic need. Dequeue via the mutation, not `--disable-auto`; check
  `isInMergeQueue`, not `autoMergeRequest`.
- During implementation, run the smallest local checks that prove the changed
  behavior and rerun them after the fix. Do not repeat a full workspace battery
  after every small correction or locally duplicate required broad CI without a
  named local-only risk.
- Required broad CI must pass on the exact final tree the platform merges.
  Focused local validation and independent review do not waive or narrow it.
  Reuse green evidence only for an identical tree with the same configuration.
