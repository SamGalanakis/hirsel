# Landing a lane

Run the tests your change can break, commit it, then run:

```sh
tools/land > /tmp/<lane>-land.log 2>&1; echo EXIT $? >> /tmp/<lane>-land.log
```

`tools/land --withdraw` stops this checkout's queued or running land after verifying its recorded process start time.

Land first joins a FIFO ticket queue shared by every worktree under
`$(git rev-parse --git-common-dir)/land-queue/`. Only its head fetches,
rebases, checks and pushes, and it lands the waiting tickets with its own
change as one batch (see [Batches](#batches)). A short `flock` protects an atomic sequence
counter and atomic JSON ticket publication; increasing numeric ticket
filenames define arrival order. A ticket keeps its place across rejected
push retries and re-execution into an updated `tools/land`. Exiting releases
it, including red checks and rebase conflicts.
Waiting lanes print their position and the head's lane name when either
changes, and at least every 30 seconds.

Each ticket records its lane, worktree, PID, wall-clock start time, Linux
process start ticks, boot ID and heartbeat. A background thread renews the
heartbeat every 10 seconds, including during blocking checks; waiting lanes
renew too, so a long queue does not expire them. Re-execution preserves the
PID and ticket and restarts the heartbeat thread. The next waiter removes
and logs a head whose process is dead (including zombies, reboot or PID
reuse), or whose heartbeat is older than 15 minutes. Process identity uses
`/proc/<pid>/stat` and boot ID, never command-line matching. It does not
signal or kill another lane. A resumed holder verifies ticket ownership
before starting an attempt and immediately before any push; a revoked or
expired holder refuses to push.

The ordering follows the increasing-ticket/acquire-head/release structure
of Linux's [generic ticket lock](https://github.com/torvalds/linux/blob/7be4bc52fd9e0e55dc9a2e0232b89427d2c4b614/include/asm-generic/ticket_spinlock.h#L33),
lines 33–49 and 63–69. Files and a short OS lock replace in-memory atomics;
sleeping waiters replace spinning; process identity and heartbeats add
recovery for abandoned tickets. These adaptations serve independent
processes and survive a lane crash on this single Linux host.

Land fetches and rebases onto a captured `origin/main` commit, regenerates
stale build inventories, and checks the workspace build, Clippy and fmt.
The build covers workspace compilation, including test harness compilation;
it excludes test execution, campaigns, the two-host link check, std images
and native std precompilation/packaging. The latter are integration artifacts
covered by main-runs, and never replace the workspace build. At this
change, the build query selects 200 targets, including all 84 Rust test
harness build targets, `hirsel-cli-bin-hirsel` and `hirsel-rt`. Compared with
the old query on the same inventory, exactly `hirsel-std-objects` is removed;
packaging targets were already excluded through the std image.
Build, Clippy and fmt always check their full aggregates.
It pushes the exact checked commit SHA, after verifying that HEAD and the
worktree still match. A red check refuses the push. Source conflicts stop
and abort the rebase; resolve them yourself and run land again. Land does
not run tests or commit generated files. Main's scheduled run owns the API
index; the generated-file and independent-line-list rules still apply.

Before publishing a ticket, land finds and probes Cargo and rustc locally;
an unavailable toolchain refuses submission with one line. Checks and
`prepare.py` share the lookup: the prepared compiler tree, the pinned rustup
toolchain under `RUSTUP_HOME` (default `~/.rustup`), then `CARGO_HOME/bin`
(default `~/.cargo/bin`), with the host PATH as a fallback. Every check
subprocess receives that explicit PATH, including nested preparation and
build-file generation, so the batch head needs no shell Rust setup.

When every new main commit touches only `tools/known-failures.tsv`, a
previously passed batch rebases and pushes without running any check again,
including the fast screen. The checked tickets with unchanged submitted heads
and live queue ownership finish first; newcomers wait for their own batch.
If tickets leave or change heads, reuse requires the remaining group to be
a prefix of the checked group, and its rebased tree to differ from the checked
tree only in ledger rows. Otherwise the remaining group is checked again.
The rebase and worktree must be clean. The normal union
merge preserves both sides' ledger rows. History is inspected commit by
commit: a source edit followed by its revert still requires checks.

Every other retry checks its rebased tree. There is no third-attempt exception,
no inherited lint environment flag and no attempt limit. Losing a push race waits a
random duration in `[0, min(30, 2**attempt)]` seconds, then fetches and
rebases again. Non-race push errors stop with the error. This is the full
jitter algorithm from
[aws-arch-backoff-simulator](https://github.com/aws-samples/aws-arch-backoff-simulator/blob/master/src/backoff_simulator.py#L40),
lines 24–25 and 40–43, with a one-second base and a 30-second cap. The
exponent is bounded before calculation because the number of retries is
unbounded. The lane retains its FIFO place during this wait; the short metadata
lock is released. External writers that do not use the queue can still
cause a rejected push.

## Batches

A ticket records the commit it lands (`head`). When the head has rebased
its own change, it claims up to 15 live tickets behind it, oldest first, so
a batch holds at most 16. It replays each ticket's commits after the
captured `origin/main`, in ticket order, onto the tree of the tickets ahead
of it (`git rebase --onto <batch> <main> <head>`), detached in the head's
own worktree. It reads other lanes' commits by SHA from the shared object
store and never touches their worktrees or branches. The checks (hot files,
regenerated build files, build, Clippy and fmt) run once on the combined
tree, in the head's own worktree, and one push carries every commit. Each lane's commits stay separate,
with their authors; nothing is squashed. Each landed ticket gets its own row
in `hirsel-landings.jsonl`.

The head writes each waiting ticket's outcome into its ticket file. The
waiting `tools/land` prints it and exits with its code: 0 with its own
landed SHAs, after which its branch points at its last landed commit; 1
with its conflict or its first error. Commits made after queueing do not
land; land says so, and you run it again for them.

A ticket whose commits do not replay leaves the batch with the conflict
message a single land prints; the rest stay on the tree before it. A
conflict with origin/main is final at once. A conflict with batched changes
ahead of the ticket stands only if every one of them lands; otherwise the
ticket stays queued for the next head. A change that touches generated
files is refused at once.

Before any build, the head screens a batch of two or more with the checks
that take seconds: build-file sync, independent line lists and fmt. These
are the full checks' own scripts. If the combined tree fails, and
origin/main passes them, a binary search over the batch's cumulative trees
finds the first ticket that turns them red. That ticket leaves with its
first error, and the rest are screened again. A fmt slip therefore costs
seconds, not a build and a bisection.

Groups of tickets are checked one at a time, each on the tip pushed so
far, and a green group is pushed at once. A red group splits into halves,
checked in ticket order. A ticket is blamed only when a check of its
change alone, on a green tree, is red; the culprit exits with that check's
first error, and the rest land. A red that does not repeat blames nobody.
A batch of one is a single land.

Every pushed tree has passed checks, reused only for row-only moves as above. Before the push, the
head confirms that it still owns the queue head and that every batched
ticket is still held by its live process; a dropped ticket's commits never
land, and the head rebuilds the batch without it. A lost push race retries
the whole batch with the usual jitter. A dead or expired waiting ticket is
dropped from the batch and removed. If the head dies, its waiting tickets
keep their places, and the next head batches them. A revoked head neither
pushes nor delivers outcomes. If the head's own change is a culprit, it
still pushes the green rest; after a lost race it exits instead, and the
next head lands the rest. A head whose own change landed in an early push
exits 0 if a later push loses its race; the tickets not yet pushed keep
their places. A row-only main move can reuse the batch's prior pass as above.

A ticket queued by an older `tools/land` has no `head`. The batch takes the
tip of the branch that its worktree has checked out, as `git worktree list`
reports it. That waiting process never reads its outcome. When it reaches
the head, its rebase drops the commits that already landed, and it exits 0
with "nothing to land".

The design follows four merge trains:

- [bors-ng](https://github.com/bors-ng/bors-ng/blob/ca725797e53a88e954998de0bbb14a8a5acb13ab/lib/worker/batcher.ex#L10)
  builds every waiting change as one batch and bisects a red batch into
  halves (`lib/worker/batcher.ex` lines 10–22, `lib/worker/batcher/divider.ex`
  lines 9–27 and 89–94). Its `max_batch_size` is the cap.
- [Zuul's dependent pipeline](https://opendev.org/zuul/zuul/src/commit/3daef6c4f92672347ba9a2dcabef4415e35f7ac1/zuul/manager/__init__.py#L1955)
  tests each change on the changes ahead of it. A failing change leaves,
  and the rest move onto the nearest non-failing item (lines 1955–1972).
- [GitHub's merge queue](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue)
  groups up to a maximum number of pull requests per merge group and
  removes only the failing one.
- [LUCI CV](https://chromium.googlesource.com/infra/luci/luci-go/+/f46b764fe10a5595c02d8779d520bbfec9705fe4/cv/internal/tryjob/execute/retry.go#43),
  the Chromium CQ, gives each verdict to one change, and retries failed
  tryjobs before it blames that change (lines 43–60).

Unlike bors-ng, land never blames by elimination, and it does not queue a
separate batch for each half: one process settles the whole batch, so the
lanes behind it keep waiting. Unlike Zuul and GitHub, it checks one tree at
a time: no concurrent land slots and no speculative batches. On the landing
record of 2026-10-24 to 2026-11-02 they paid on one busy day, 2026-10-25,
and once after it (see [Mechanisms on the record](#mechanisms-on-the-record)).

## Mechanisms on the record

Measured from `hirsel-landings.jsonl` (rows since batching began,
2026-10-24 13:46 UTC) and the 1,462 `/tmp/*-land.log` files written since.
Land logs carry each check's duration but no timestamps, and a lane's log is
overwritten by its next land, so log counts are lower bounds.

| Mechanism | Fired | Wall clock saved | Kept |
| --- | ---: | --- | --- |
| Ticket queue | every land | not measurable (it is what batching claims from) | yes |
| Batching | 996 of 1,381 tickets in 239 multi-ticket pushes | 757 full checks not run | yes |
| Fast screen | 64 heads found a fmt or line-list red | a red batch build and bisection per firing | yes |
| Bisection | 25 red batches | 112 full checks fewer than checking each ticket alone | yes |
| Row-only check reuse | 11 retries | 57–986 s each, about 6,500 s in all | yes |
| Several land slots | 9 heads checked trees at once (8 on 2026-10-25) | at most 23,700 s of overlap, before the rechecks it caused | no |
| Speculative batches | 6 heads, all on 2026-10-25 | inside the slot overlap above | no |
| Narrowed retries | 0 | 0 | no |

## Regression test

```sh
python3 tools/buck2/sync.py --if-stale
.buck2/bin/buck2 test -c hirsel.tests=host //:test-land
```

The host setting supplies Git, which pool workers need not have.
The test uses a bare local Git remote, real rebases and pushes, and fake
build tools. Three concurrent worktrees queue behind a deliberately slow
first check.
The remote's receive hook requires build, Clippy and fmt records for the
exact proposed SHA and base; all three land in ticket order, the first
alone and the other two as one batch. Killed
and expired live holders are reaped; a resumed holder refuses to push
after losing its ticket. Re-execution retains the ticket and PID identity.
Batch cases: three clean changes
land with one check run; a fmt-red change in a batch of four leaves in
the screen, before the one build; a build-red change in a batch of four
bisects in five builds, and the green half lands first; a
conflict in the middle ticket leaves only that ticket; a conflict with a
red ticket ahead stays queued and lands next; a dead lane's ticket is dropped; a batch that
loses a push race is checked again and lands whole; a passed batch rebases
over repeated row-only main moves with zero further checks and unions the
rows, even with newcomers queued during its check; newcomers get their own
checks. Departed tickets require a recheck unless the surviving prefix differs
only in ledger rows. Source moves (including an edit and revert) still re-check; and a ticket queued
by an older land is batched and later finds nothing to land.
Further cases cover
22 rejected attempts, a red retry in each aggregate, worktree/HEAD edits,
legacy lint-pass environment variables and an empty build query.

Only the test harness removes the randomized wait. Barrier waits and child
processes have bounded deadlines so a broken retry cannot hang the suite.
