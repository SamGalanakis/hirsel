# Law table and law lock

`hirsel test --laws` reports law strength. `hirsel laws write` and `--write-laws`
generate the lock, and `hirsel laws check` checks its static columns.

Version note (2026-11-24, verify-phase-spec-2034): first text. This document specifies the
2026-11-22 ratified item 3 (`hirsel.laws`, the verdict table that never rounds up) as
amended by the 2026-11-24 shortlist:

- **VP3.** The engine supplies the verdict strength and the runner reports it beside
  pass and fail.
- **VP4.** `sometimes` witnesses are explicit, and the runner flags vacuous runs.
- **Two checks.** Lock currency, which runs no tests, is separate from verdict strength.
- **`--locked`.** Its meaning is unchanged.
- **Locked bounds.** Only count bounds are locked.
- **`/// bug:`** stays prose.

These are out of scope:

- the `grounded` flag;
- any checkable `/// bug:` form or mutation strength column (VM2);
- any `Closed` or `prove` verdict (VM5, Phase 2);
- shared verdict receipts (§4);
- an expected-red registry;
- automatic reach counters at each assertion.

This document, like [recording](recording.md), is a tool contract and not part of the
language. Under R28 ([reserved](spec/reserved.md)), no row, lock line or baseline is an
input to acceptance, lowering, code identity, replay or any observable of
[SEM-8](spec/semantics.md#sem-8-determinism), and no program reads one. `check`, `build`
and `run` never read `hirsel.laws`. It is not part of any PROG-10 address or package
digest. Under R29 nothing here adds a cause kind: a failing law still raises `Fail(text)`,
and rows and comparisons are runner output, never failures.

## Two checks with two homes

| check | what it runs | cost | where its red goes |
|---|---|---|---|
| **Lock currency** (`hirsel laws check`) | the front end only; no test runs | a table comparison after checking, target under 1 s with a warm check cache | a policy red at land time, like a stale lock |
| **Verdict strength** (`hirsel test --laws`, `--laws-baseline`) | the tests | the tests' own cost | the lane's one test for the laws it touched, and the nightly board for the rest; a red is routed to an owner and is not a pre-push gate |

`hirsel test --locked` keeps its meaning: it requires a current workspace lock
([packages](spec/packages.md)). Nothing in this document adds to it.

Version note (2026-11-25, laws-land-lint-2703): land lint is
`tools/buck2/fmt.py --check`. It builds the compiler from the tree being checked
and runs `core/tools/law_board.py check --changed-since origin/main`, which runs
`hirsel laws check PKG --libraries core` on each lock package (a member of
`core/programs/hirsel.toml` or a directory with a tracked `hirsel.laws`, such as
the `*/vectors` packages) that a change since the merge base can move: a changed
path inside the package or inside a package it reaches through `path`
dependencies. A change to `core/std`, `core/core`, the workspace manifest or
lock, `law_board.py`, or a crate source that computes a lock's hashes
(`law_lock.rs` and `law_lock/`, `reuse.rs`'s header and signature digests,
`native_entry.rs`'s policy tuple, `hirsel-core-ids/src/lib.rs`, and
`hirsel-core-syntax`'s `token.rs`, `lexer.rs` and `lib.rs`) checks every lock
package. Without `--changed-since`, `law_board.py check` checks them all. The
nightly runs that full check, so a lock moved by any other compiler change is
caught within a day, then runs the members and reports law regressions in its
run log, with per-package results under `laws/`.

## Pass and fail are unchanged

[PROG-4](spec/programs.md#prog-4-tests) still decides whether a test passed:

- `Ok(())` passes;
- `Error(e)`, a failure or a run stop fails.

The **strength row** is a second column beside that outcome. A row never turns a failure
into a pass or a pass into a failure. It can make a comparison red, under
`--laws-baseline` only.

## Observation

The runner learns how strong a law's evidence is by **observing the settled outcomes
of outermost std engine calls** in the test's run. It never asks the law what it found. A law's
match arms can still fail the test, which makes the law stricter, but they cannot raise
its row.

**Observed functions.** The observer identifies these by definition identity
([PROG-5](spec/programs.md#prog-5-code-identity)):

- `std.prop.search`, `check`, `all`, `check_tasks`, `check_corpus` and
  `check_tasks_corpus`;
- `std.prop.sometimes`;
- `std.model.explore`, `check`, `conforms` and `conforms_check`
  ([model](../core/std/api/model.md)).

When one engine call invokes another, for example `check` calling `search`, the observer
records the outermost outcome. Explicit `sometimes` witnesses count even inside an engine call.

**What is observed.** The observer reads the outcome when the engine settles it, on
return or, for a failing check, immediately before the engine raises its own failure:

- the outcome the function computed, with its counts;
- the configuration fields the outcome depends on: `Config.cases`, `max_choices`,
  `max_rejects` and `gen_steps`, or `Bounds.depth` and `states`;
- the `FailureClass` of a `Found` (`Returned`, a cause kind, `OutOfTime` or `Capacity`);
- for `sometimes`, the label and whether `cond` was true.

Version note (2026-11-24, verify-observer-2037): specifies where a check that
raises a failure yields its typed outcome to the observer. The outcome is attributed
to the outermost check call.

**The observer changes nothing in the program.** The observer obeys CAP-9's rules for a
recording:

- it charges no limit, takes no step, and adds no event, poll or provider operation;
- it changes no delivery;
- nothing it receives is built in program space;
- when it fails, the run goes on unchanged and the row is `unobserved`.

Law observation is part of the recording build ([recording](recording.md)). It runs when
that build starts with `HIRSEL_LAWS=PATH`, with `HIRSEL_RECORD` set or not, and it writes
its rows to PATH. A build without the recording switch contains none of it, so a build
with laws off pays nothing, at compile time or run time. That keeps CAP-9's single build
switch. A test whose program reaches a foreign function has no recording build
([FFI-8](spec/foreign.md#ffi-8-a-program-that-reaches-foreign-code-is-never-recorded-replayed-or-reused)),
so its row is `unobserved (FFI-8)`.

**Provisional mechanism.** This mechanism must be built before the rest of 1.3 starts, as
build item V13-1. The item is accepted when:

- the rows match the returned outcomes on every `std.prop.all` and `search` case in
  `core/std/tests/prop.hirsel`;
- a law run with laws on and with laws off makes the same SEM-8 observations, checked in
  the existing recording same-observation harness.

**Fallback.** If the prototype cannot meet both conditions without changing the
signature or behaviour of `all`, 1.3 falls back as follows:

- laws keep their explicit match arms;
- `hirsel check` lints a wildcard or binding arm that returns `Ok` on an `AllOutcome`,
  `std.model.Outcome` or `Conformance`;
- the lock keeps only its static columns.

That fallback needs its own spec amendment before it is built.

## Rows

A row describes one **call site**: the observed function, the definition whose body holds
the call, and the call's ordinal among that definition's calls to that function, in source
order. The definition may be a test, a test-source helper or a production function. A
helper called from several tests gives one site per test, since rows are kept per test. Calls from one site, from any task, are aggregated by
commutative operations:

- the class is the weakest class observed (below);
- the numbers are those of the weakest observation, ties broken by the smaller numbers;
- `sometimes` counts are summed.

So a row never depends on the order in which tasks complete
([CONC-10](spec/tasks.md#conc-10-event-order)).

Classes, weakest first, for each family:

| family | classes |
|---|---|
| sampled (`search`, `check`, `check_tasks`, corpus variants) | `falsified` < `gave up (cases, rejected)` < `vacuous` (`Passed(0)`) < `sampled (cases)` |
| exhaustive (`all`) | `falsified` < `unbounded (at)` < `vacuous` (`Held(0, d)`) < `truncated (run, discarded; bound)` < `held (run, discarded)` |
| model (`explore`, `check`) | `failed (kind, depth)` < `to depth (k; limit)` < `closed (states, edges, depth)` |
| conformance (`conforms`, `conforms_check`) | `diverged (entry)` < `incomplete (entry; limit)` < `conforms (entries)` |

**Vacuous runs.**

- A run that checked no case is `vacuous`, never `held` or `sampled`. That covers
  `Held(0, d)` with every path discarded, and `Passed(0)`.
- A `std.model` walk cannot be vacuous, because `init` is never empty and every model has
  a `sometimes`.

A swallowed result shows in its row. A law with a catch-all arm, `_ => Ok(())`, that
receives `Truncated` passes, but its row is `truncated`. A law that never reaches its
engine call has no row at that site.

**Bound and source.** Every row except `held`, `closed` and `conforms` prints the bound
that stopped it and the field it came from: `cases=512`, `max_choices`, `gen_steps`,
`depth=6` or `states=10000`.

**Bounds that differ per build (MEM-30 Q1).** A row whose class was caused by a step or
byte bound is a fact about one build. It is printed with `(per build)`, written to the
lock without numbers, and never compared across builds. These rows are per build:

- `truncated` by `gen_steps`;
- `falsified` with class `OutOfTime` or `Capacity`;
- a model `failed` of kind `Handler`.

`truncated` by `max_choices` cannot be told apart from `gen_steps` by the returned
`Truncated(run, discarded)` alone. Both leave `run + discarded < cases`, so both are
per build. Every other row is a count row: the same on every engine, and lockable.

**Witnesses.** `pub fn sometimes(label: String, cond: bool) -> bool` in `std.prop` returns
`cond` unchanged. It is ordinary code: its call is the same in every build and costs one
step in every build. The observer counts, per test and label, the calls where `cond` was
true. A law opts in by calling it, and `sometimes` is never required in `std.prop`. A test
with no `sometimes` shows `no reach declared`, which is not red. A `std.model` model
declares its own `sometimes`, which are required there, and they appear in its row as
`witnessed`.

**Reused passes.** A pass reused under PROG-4 has no run to observe. Under `--laws`, the
runner reuses a pass only when the pass was recorded with its rows, and then it reports
those rows. A row is as deterministic as the pass: same program, same configuration, same
build. Otherwise the test is executed.

## Output

`hirsel test --laws` adds a strength column, and a line per row below each test:

```text
ok   store.tests.search_space   executed   held 512/0
       all  search_space#1  held (run 512, discarded 0)
       sometimes  hit_limit  37
ok   store.tests.swallowed      executed   truncated
       all  swallowed#1  truncated (run 100, discarded 0; cases=100)
ok   store.tests.model_lease    executed   to depth 6
       model.explore  model_lease#1  to depth 6 (depth=6); witnessed elected 4
```

The test's strength is its weakest row, with `none` when it has no rows. `--json` adds a
`laws` array per test, where each element has `callee`, `site`, `class`, `numbers`,
`bound`, `per_build` and `witnessed`.

Version note (2026-11-24, verify-observer-2037): implements call-site rows, JSON output
and pass reuse with recorded observations.

## The lock file

`hirsel.laws` sits beside each package's `hirsel.toml`. Its first line is
`hirsel.laws 1`, followed by one line per `@test`, sorted bytewise by full definition path.
With one sorted line per law, two lanes that touch different laws merge as text.

```text
hirsel.laws 1
store.tests.search_space src=1f0c3a9e5b7d2c41 sig=88a1e0f3c2b4d6a7 run=0b9c8d7e6f5a4b3c bug=5e2d… oracle=- rows=all#1:held:512/0;sometimes:hit_limit
```

Fields:

- **`src`.** The first 8 bytes, in hex, of SHA-256 over every test-source definition
  reachable from the test without passing through a production definition. Each
  contributes its full path and its source bytes without doc comments, in path order.
- **`sig`.** The same, over the PROG-5 identity and checked signature digest of each
  production definition those definitions name directly, in path order.
- **`run`.** The same, over the test's effective run configuration from `hirsel.toml`:
  root limit, grants, directories and environment values.
- **`bug` and `oracle`.** The same, over the text of the test's `/// bug:` and
  `/// oracle:` doc-comment lines, or `-` when the test has none. They are prose. Nothing
  parses them further.
- **`rows`.** The count rows, as `callee#ordinal:class:numbers`. A per-build row is
  written `callee#ordinal:class:per-build`. `sometimes:LABEL` lists each label with a
  nonzero count, and its count is not locked. `-` means none.

Each `src` and `sig` input is framed by its length in eight little-endian bytes.
`run` hashes the canonical JSON tuple of the limit, grant mask, network grants,
credentials, environment selection, directories, foreign grants and exec grants.
A call in a helper uses `callee@DEFINITION#ordinal` to keep sites in different
definitions distinct. A falsified row records its failure class as `falsified/KIND`.

**Commands.**

- **`hirsel laws write PKG`** recomputes `src`, `sig`, `run`, `bug` and `oracle` and adds
  or removes lines for added and removed tests. It runs no test. A line whose `src` or
  `sig` changed, and a new line, gets `rows=pending`; its old rows are dropped. Only a
  `--write-laws` run replaces `pending`.
- **`hirsel test PKG --laws --write-laws`** runs the tests and rewrites the `rows` of every
  test it ran. Other lines are left alone, so a filtered run rewrites only its own lines.
- **`hirsel laws check PKG`** is lock currency. It exits 1 if writing would change any
  static column or any line's presence. Rows are never compared here.

## Baseline comparison

`hirsel test PKG --laws-baseline FILE` implies `--laws` and compares each observed row with
the line for the same test and site in FILE.

FILE is the `hirsel.laws` committed on main. For the nightly board use the copy at
the last complete board's commit, for example `git show COMMIT:pkg/hirsel.laws`.
`hirsel laws write` and `hirsel test --write-laws` generate the working copy and
follow the normal landing workflow.

Version note (2026-11-24, verify-observer-2037): Sam confirmed that `hirsel.laws`
is an ordinary generated lock file. The nightly uses the last complete board's
commit on main as its comparison point.

A row is **red** when:

- **Weaker class.** Its class is weaker than the baseline's in the family order.
- **Fewer numbers.** It has the same class and a smaller count: `run` for `held`, cases
  for `sampled`, states for `closed`, entries for `conforms`, or `k` for `to depth`.
- **Negative row changed.** The baseline's class is a negative one: `falsified`,
  `failed` or `diverged`, from a law that asserts its own counterexample. Any change of
  class or kind is red.
- **Witness lost.** A `sometimes` label in the baseline is missing or has count 0.
- **Site missing.** A baseline site is missing, or the test is missing.
- **Per-build row.** The baseline holds a count row and the run produced a per-build row
  instead.

A baseline per-build row is satisfied by any row except a negative one. A line marked
`pending` in the working tree is compared against FILE all the same. A stronger
row passes the comparison.

**Narrowed runs are refused.** `--laws-baseline` refuses `--filter`, `--exact`, `--seed`, `--sim`,
`--seeds`, `--property-corpus` and `--replay` with
`test: --laws-baseline compares a full run; drop OPTION`, and exits 2 as any refused
argument does. A focused run uses `--laws` and
reports its subset. It cannot certify the full inventory.

The comparison prints each red row with the baseline beside it and exits 1 when any row is
red. A failing test still fails as PROG-4 says.

## Laws for this phase

Each law's expected value is derived by hand from the `std.prop` text and this document.
Cases go in `crates/hirsel-core-driver/src/test_run_native.rs`, the package-law harness
that already runs `hirsel test` on small packages.

| # | Package test | Expected row | Bug it catches |
|---|---|---|---|
| L1 | `all(c, u64_in(0, 9), ok)` with `cases = 5`, arm `_ => Ok(())` | test `ok`, row `truncated (run 5, discarded 0; cases=5)` | a catch-all arm passing as `held` |
| L2 | `all(c, filter(u64_in(0, 3), never), ok)`, arm `Held(_, _) => Ok(())` | `vacuous (Held(0, 4))` | an all-discarded run reported as `held` |
| L3 | `all(c, u64_in(0, 9), ok)` with `cases = 100`, and again with `cases = 10` | `held (run 10, discarded 0)` both times (prop: reaching the case limit on the final leaf still returns `Held`) | a wrong count, or the final leaf at the limit reported as `truncated` |
| L4 | L3, baseline `held 10/0`, then the generator narrowed to `u64_in(0, 4)` | `--laws-baseline` red: `held 5/0` below `held 10/0` | a narrowed law staying green |
| L5 | a test calling `sometimes("big", x > 100)` over `u64_in(0, 9)` | `sometimes big 0`; red against a baseline whose line lists `sometimes:big` | a lost witness staying green |
| L6 | L3 under `--laws-baseline` with `--filter x` | refused, exit 2 | a filtered run certifying the inventory |
| L7 | L3 run twice, the second time reused | the reused row equals the executed row | reuse dropping the row |
| L8 | `check_tasks` law with 4 cases, each case spawning one task that calls `sometimes("t", true)` once | `sometimes t 4` on every `-j`, under the same build | a row depending on task completion order |
| L9 | `hirsel laws check` after editing one test body | exit 1, naming that test's `src` | lock currency missing a body change |
