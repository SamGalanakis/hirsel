# Containment program generator (internal)

**Status: design only. Nothing here is built yet.** This document covers
implementation and test tooling. It is not part of the language specification. It describes a
seeded generator of well-typed Hirsel programs that use `bound steps`, `bound bytes` and
`isolate`, the oracles that judge each generated program, and the nightly campaign that runs
them. The spec rules it checks are
[MEM-19](../spec/budgets.md#mem-19-limits), [MEM-24](../spec/budgets.md#mem-24-logical-charges),
[MEM-25](../spec/budgets.md#mem-25-the-bound-scope),
[MEM-28](../spec/budgets.md#mem-28-charges-are-an-upper-bound),
[MEM-30](../spec/budgets.md#mem-30-step-bounds),
[FAIL-15](../spec/failure.md#fail-15-run-stops), [FAIL-16](../spec/failure.md#fail-16-isolation)
and [FAIL-17](../spec/failure.md#fail-17-stack-exhaustion).

## Why

Hand-written probes found the step-bound escapes E1 and E2 (containment study 3202):

- **E1:** engine-generated clone and `==` took no steps.
- **E2:** generated clone recursion stopped the whole run from inside `isolate { bound … }`.

Neither was in any law, and no differential could have seen E1. The 3197 work-step table
emits its `Step` instructions from one front helper for all three engines, so a charge the
front omits is missing everywhere and the engines still agree. The generator is meant to find
the next class of this kind before a user does. It generates programs whose containment
outcome follows from the spec, and checks each program against oracles that do not depend on
the code under test.

## Shape

```text
seed ─► generator (choice stream) ─► package + predictions ─► hirsel check
                                                             │ refused → generator red
                                                             ▼
         evaluator   Cranelift dev   LLVM dev   LLVM release  (one compile per batch per build)
                │            │            │            │
                └────── tag streams, step and byte reports ──► oracles O1–O5
                                                             │ red
                                                             ▼
                                minimise ─► dedup key ─► alert line + issue + repro package
```

The generator, the reducer and the campaign driver are Python modules beside the std
contract runner: `core/tools/containment_gen.py` and `core/tools/containment_fuzz.py`. Both
are new files, created by slice C1 below. They use the same driver, evaluator and native build
paths that `core/tools/std_native.py` already uses. The work tally (O3b) and the static cycle
check (O3c) are additions to `crates/hirsel-core-eval`. Like the 3226 ceiling, they are
switched on by an environment variable, `HIRSEL_TEST_WORK_TALLY`, as `Work::check_ceiling`
reads `HIRSEL_TEST_WORK_CEILING`. No product code changes.

## Generator

### Vocabulary

Each program is a package with a fixed number of entries (`case_0` to `case_{M-1}`, selected
by an argv index). Every entry follows one template:

1. **Build inputs outside the scope.** Construct values of generated types at a drawn size,
   using constant-trip-count loops. Every size is known to the generator.
2. **Enter a contained scope.** This is one of the nestings `isolate { bound bytes b { bound
   steps n { body } } }`, `isolate { bound steps n { … } }`, `bound steps n { isolate { … } }`
   or `bound bytes b { … }`, with up to three levels of nesting.
3. **Run the body.** The body draws on these forms:
   - loops with static trip counts and data-dependent loops over inputs;
   - direct and mutual recursion over generated recursive types;
   - `clone`, `clone_in` and structural `==` and ordering over generated types;
   - the core operations in the MEM-30 work table: `list.append`, `copy_from_slice`,
     `copy_within`, `fill`, `compare_bytes`, `string.push`, `list.sum`, `deque.remove`,
     `columns.fill`, `int.parse`, `string.from_utf8`, base64, ASCII case, `crc32c.update`,
     the SHA rows and the HMAC rows;
   - `task.spawn` and `task.join` with a byte grant, under the current bound;
   - nested `bound` and `isolate` scopes.
4. **Report the outcome.** The scope's outcome becomes one record in the entry's **tag
   stream**. A scope result is a sink: it is printed and never feeds any later computation.
   This keeps one tolerated divergence (O2) from masking the scopes that follow it.

The generated types are records, sums, `Box`-recursive chains and trees, `List` of any
generated type, `String`, `Bytes`, `Map` with `u64` keys, `Deque` and `Inline`. A type
derives `key` (and `ordered`) only when a production needs `==` or ordering on it, because
the checker refuses `==` on a type that is not `key` (LANG-21).

The vocabulary excludes, by construction, every FAIL-15 stop other than the one under test.
That gives O1 a closed world:

| FAIL-15 stop | How it is excluded |
|---|---|
| `Deadlock` | every spawn is joined in the scope that spawned it; no channels, `Latest` or virtual providers |
| `CountOverflow`, `ChannelSenders` | no counted value gets more than `2^20` holders; no channels |
| host refuses memory | the root limit and every byte grant stay below the 2 GiB measured-process cap |
| provider ABI break | no providers; the only capabilities are `Stdout` for the tag stream and `Tasks` |
| termination by the host | none expected; a signal exit is a harness red, filed separately from O1 |
| `Stack` (engine reserve) | **not excluded**: this is the E2 class that O1 hunts |

Floats are left out of version 1, so that NaN payloads cannot enter the agreement relation.
Wall-clock reads, environment and file access are also left out, so every entry is a
function of its source and argv (SEM-8).

### Well-typed by construction

The generator follows wasm-smith's design: every production has a **validity predicate**
over the current typing environment and the expected type. Only productions whose predicate
holds can be chosen. The environment records, for each name in scope:

- its type and whether it can be moved, viewed or used `mut`;
- whether it was declared outside the innermost `bound steps` or `isolate`;
- whether the enclosing function and every enclosing callback is `waits`.

The rules the predicates encode, each with the rule that requires it:

- **MEM-30 "Places outside" and FAIL-16:** a body never assigns, replaces or lends `mut` a
  place declared outside its scope. To change an outer value, the generator uses the spec's
  pattern of moving a working value in and returning it with the result.
- **MEM-30:** a callback invoked inside `bound steps` captures only places declared outside
  the scope, read-only. The checker refused a bound body in which a nested `isolate` callback
  captured a local of the bound body next to a spawn and join. The narrow rule stays until
  the exact condition is pinned against the checker.
- **LANG-8:** a callback is `waits` exactly when its body spawns, joins or calls something
  that `waits`. Over- and under-declaring are both refused.
- **LANG-12:** every integer literal whose type a generic call leaves open gets a suffix.
- **LANG-21:** `==` and ordering apply only to `key` and `ordered` types.
- **LANG-2:** names are drawn from a generated alphabet that never hits a reserved word.

`hirsel check` remains the arbiter. A generated program the checker refuses is a
**generator red**, filed under its own key with the refusal text. It is never dropped
silently, and the nightly report counts these refusals. A refusal the generator's rules call
wrong is evidence of a checker bug and is filed the same way. The media toolchain accepted
this template program. With a `main` that prints `g0(build(1000), xs) + g1(tasks)`, the run
printed `3`. The fence below omits that `main`:

```hirsel
type Chain: key = Nil | Link(u64, Box[Chain])

fn build(n: u64) -> Chain {
  let mut c: Chain = Nil
  let mut i: u64 = 0
  while i < n { let prev = take c; c = Link(i, box.new(take prev)); i += 1 }
  c
}

fn child(take a: (u64, u64)) -> u64 {
  let (x, y) = take a
  x + y
}

fn g0(c: Chain, xs: List[u64]) -> u64 {
  let r = isolate(fn() -> u64 {
    let b = bound bytes 65536 {
      bound steps 1000 {
        let d: Chain = clone(c)
        let e = d == c
        let inner = isolate(fn() -> u64 { list.len(xs) })
        let k: u64 = match inner { Ok(Ok(v)) => v, Ok(Error(_)) => 3, Error(_) => 4 }
        if e { k } else { 7 }
      }
    }
    match b { Ok(Ok(v)) => v, Ok(Error(_)) => 2, Error(_) => 5 }
  })
  match r { Ok(Ok(v)) => v, Ok(Error(_)) => 9, Error(_) => 8 }
}

fn g1(tasks: Tasks) -> u64 waits {
  let b = bound steps 1000 {
    let h = task.spawn(tasks, bytes 4096, (1u64, 2u64), child)
    match task.join(take h) { Ok(v) => v, Error(_) => 0 }
  }
  match b { Ok(v) => v, Error(_) => 2 }
}
```

### Seeds and reproducibility

- **The choice stream.** The generator makes every decision by drawing from a **choice
  stream** of `u64` values. A seed expands into the stream through SplitMix64, the generator
  `crates/hirsel-core-net/src/split_mix.rs` already uses for scripted schedules. The generator
  implements it in Python itself. It does not use `random.Random`, whose methods other than
  `random()` are not guaranteed stable across Python versions.
- **The address of a program.** A program is addressed by `(generator version, policy, seed)`.
  The generator version is a constant in `containment_gen.py`, bumped whenever any
  production changes, so an old seed is never replayed through a different generator.
- **The repro.** The reproduction of any red is the emitted package itself, its source and
  argv, so replaying it needs neither the generator nor the seed.
- **Policies.** Each batch draws a **policy** (YARPGen's generation policies; swarm testing,
  Groce et al. 2012). A policy is a random subset of the productions, plus weights and a size
  class. This lets rare combinations, such as deep recursion with a nested `isolate` and a
  spawn, come up often instead of being diluted by every other form.
- **Seed ranges.** Nightly ranges come from the same SQLite ledger that the VOPR campaign uses
  (`tools/sim-campaign`'s `allocate`), under the campaign name `containment-gen`, so nights
  never repeat a range. Seeds that were red on an earlier night are replayed first, as
  `reds_first` does for laws.

### Sizes

E1 needed inputs of 500k elements under a bound of 1000. E2 needed recursion depths of 60k
to 300k. A byte-driven generator tends to produce tiny values, so sizes are explicit and are
drawn from **size classes**:

| class | element counts, string bytes, recursion depth | engines |
|---|---|---|
| small | log-uniform in `[1, 2^12]` | evaluator, Cranelift dev, LLVM dev, LLVM release |
| large | log-uniform in `[2^16, 2^20]` for counts and bytes, `[2^15, 2^19]` for depth | the three native builds; the evaluator runs the same entry at a small size |

Every size is an argv parameter of its entry. The evaluator can therefore run a large-class
entry at a small size, while the natives run it at both sizes. The large class keeps off the
evaluator for two reasons:

- its instruction budget (`INSTRUCTIONS = 10^9` in `crates/hirsel-core-contract-eval/src/main.rs`);
- its call recursion, which runs on the host stack.

The bound `n` is drawn log-uniform in `[1, 2^16]`, then nudged so that the predicted floor
`L` (O3a) lands on either side of `n` about equally often. This is the probes' "big input
outside, tiny bound inside" shape.

## Oracles

Each oracle names its independent source. None of them uses a build's own output as its
expected value. MEM-30 Determinism says steps differ across builds, so no oracle compares
step counts across engines for equality.

### O1: no run stop inside a contained scope

**Rule.** No generated entry exits with status 102 or reports a stopped outcome on any build.

**Source.** FAIL-15 together with R2's oracle as 3227 states it: inside
`isolate { bound steps n { … } }`, no input may produce a run stop. Stack exhaustion in user
code is the catchable failure `Stack` (FAIL-17, landed in cf7b9d7b). After 3227, depth-driven
glue takes steps or iterates, so a bound stops it before the engine reserve runs out. The
vocabulary rules out every other stop (the table above). A run stop is therefore a red
whatever its name.

**E2 today.** Since 3197, a clone takes one work step for each 4 boxes it copies (MEM-30
work table), so a deep chain clone under a small bound now runs out of steps before it
exhausts the stack. The E2 class therefore still reaches a run stop only when `n` is above
depth/4 and the depth is past the build's stack. That is the shape the large size class
produces. Until 3227 lands, those programs are expected to be red and file once, under the
key `containment:run-stop:clone-glue`. After the fix they serve as a standing control. Codec,
format and hash glue over recursive types, which 3202 did not test, get the same shape.

### O2: outcomes agree across engines

**Observation.** Each scope prints one tag record:
`scope <id> <kind> <payload>`, where `<kind>` is one of:

- `ok` with the value's FNV-1a hash;
- `out_of_time` with the bound's `n`;
- `capacity`;
- `caused` with the sorted cause kinds;
- `isolate_capacity`.

The whole stdout and the entry's result must be equal, except where the relation below
allows a difference.

**Relation.** For each scope, take the evaluator's record as `E` and a native build's record
as `N`. Each allowance comes from spec text:

| E | N | allowed? | why |
|---|---|---|---|
| any | the same | yes | |
| `ok` / `caused` | `out_of_time`, or the reverse | **only when O3a does not force `out_of_time`** | MEM-30 Determinism: a build may take more or fewer steps than the reference count |
| `caused [Stack]` | anything else, or the reverse | yes | FAIL-17: each build fixes its own stack allowance |
| `ok` | `capacity` | **no** | MEM-28: a body that fits by its logical charges fits on every engine |
| `capacity` | `ok` | yes | MEM-28: a build may admit what the logical charges refuse |

Between native builds, the same table applies with the more conservative record standing in
for `E`.

**How the relation keeps from masking itself.** A tolerated difference changes only that
scope's record, because scope results are sinks. Each nightly report gives the share of scope
records compared exactly. A night under 80% fails the campaign as vacuous rather than passing
it.

### O3: work ≤ K·steps (R1)

Four checks, from the most independent to the least.

**O3a: predicted floor.**

- **Source.** MEM-30 Determinism: every build takes at least one step per iteration of a
  loop in user code, one per call into user code that recursion reaches, and the work steps
  "as the reference count does". The work steps come from the work table, applied to logical
  sizes under MEM-24.
- **The prediction.** While it generates, the generator computes for each scope a lower
  bound `L`:
  - loop iterations, from static trip counts and the drawn sizes;
  - recursive calls, from the drawn depths;
  - `ceil(units / quantum) + weight` for every table operation over values whose sizes it
    chose.
- **What L leaves out.** It leaves out non-recursive calls, which an engine may inline, and
  the named gaps (below).
- **The rule.** When `L > n`, every build must return `out_of_time`, with two
  preconditions:
  - O4a predicts that the scope fits its byte bound, or the scope has none. Otherwise
    `capacity` may come first.
  - For a scope that recurses in user code, `caused [Stack]` is also accepted, because each
    build fixes its own stack allowance (FAIL-17). The generator emits
  this prediction into the program as a self-check, and the entry returns `false` when it
  fails, so the same check also runs on the natives. `bound steps 1000 { clone(500k list) }`
  is the E1 case. A deep chain clone has `L ≥ depth/4` from the box row of the work table
  today. 3227's per-call glue steps raise that bound.
- **Where L comes from.** The quanta, the rows and the gap list are copied by hand from the
  MEM-30 table into `containment_gen.py`, with the spec line cited for each row. They are not
  imported from `crates/hirsel-core-ids/src/work_steps.rs`, because that is the code under
  test.

**O3b: measured tally in the evaluator.**

- **The mismatch it targets.** The 3226 ceiling compares the units that the front passed to
  the work helper against the steps debited. Glue that never calls the helper passes it
  trivially.
- **What it adds.** O3b measures work **where it is done**. With `HIRSEL_TEST_WORK_TALLY` set, the
  evaluator tallies, per open step bound:
  - (i) the bytes and elements each bulk primitive actually touches, counted inside the
    primitive's implementation and classed by its MEM-30 row;
  - (ii) the IR instructions executed.
- **The rule.** For each bound scope:

  ```text
  Σ_rows ceil(tally_row / quantum_row)  ≤  steps debited in that scope
  instructions                          ≤  S × (steps debited + 1)
  ```

  Here `S` is the longest step-free instruction path in the program's lowered IR, glue
  included, computed statically by O3c. So K is the table's quanta for bulk work and `S`
  for straight-line work. Neither is a tuned constant.

**O3c: static step-cycle check.**

- **Rule.** In the lowered Core IR of each generated program, glue bodies included, every
  control-flow cycle and every recursive call-graph cycle must pass through a step point: a
  call that takes a step, a `LoopIteration`, or a work-step `Step`.
- **Exempt bodies.** Drop glue and growth are exempt (below).
- **Effect.** A glue loop or recursion without a step makes `S` infinite. This flags E1 and
  E2 statically, with no run at all.

**O3d: the native ceiling.** The natives run with `HIRSEL_TEST_WORK_CEILING` set, so the 3226
assertion fires in the testing runtime. It is cheap, but it is relative to the helper, so it
is not counted as an independent oracle.

**What MEM-30 sanctions, and how O3 treats it:**

| sanctioned no-step work | treatment |
|---|---|
| drop (MEM-30: "Drop and growth take no steps") | drop glue is exempt from O3b and O3c; `L` counts none |
| growth | growth's copy is excluded from the tally; the appends that cause it are counted under the copy row |
| a core call outside the work table | kept out of the body vocabulary, except as a gap control |
| named gaps: `==` at a type parameter; a sequence nested in derived `==`; map key hashing and equality; key derivation; copying a large `Inline` by assignment; `==` on `Deque` and `Map`; core-source loops outside the table | each has one fixed **gap control** program that runs every night. Its O3b excess is reported as `sanctioned: <gap>` under a fixed key. If a control stops exceeding, the gap has closed or the tally has gone blind, and the report flags it so that the gap list and MEM-30 are updated together |
| spawn multiplication (E3: a child runs under its own copy of the remainder) | O3 applies per task. If the R3 ruling makes spawns reserve from the spawner, O3a gains "total across the tasks spawned under `n` ≤ `n`" |

### O4: bytes ≤ bound

- **O4a: predicted fit.** The generator computes the MEM-24 logical charge of every
  allocation in a scope. When the total is at most `b`, no build may report `capacity` for
  that scope.
  - Source: MEM-28, "a body that fits under `bound bytes n` by its logical charges fits on
    every engine".
- **O4b: native ≤ evaluator.** A native build's bytes used are at most the evaluator's. This
  is `check_metering`'s existing check (c) in `core/tools/std_contracts.py`. Since 3226,
  `std_native.py` passes the evaluator reference to `native_meter` for every native profile,
  so this check is reused as it is.
  - Source: MEM-28.
- **O4c: nothing outlives the run.** An entry ends with 0 bytes admitted: the evaluator's
  `used == 0`, and the native ledger read after teardown (3226).
  - Source: MEM-19 and MEM-23. This is the class of the H1 cell leak.
- **O4d: real memory ≤ charge.** Under the charge-observation runtime
  (`crates/hirsel-core-rt/src/peak.rs`), the native backing each scope retains is at most its
  charged bytes plus the observer's reported slack.
  - Source: MEM-28's upper-bound rule. It catches allocations that hold real memory with no
    charge, as H1's 468 B per 32 B cell did.

The root limit is set above every scope's `b`, so that a `capacity` record always belongs to
the scope's own bound.

### O5: the R4 floor

`check_metering` (`core/tools/std_contracts.py`) runs unchanged on each build:

- steps are at least the evaluator's `step_floor`;
- three runs of one build give one count;
- an injected limit refuses at one source point.

Two hazards, handled by exclusion rather than by an argument about the runtime:

- **Maps.** A native runtime keys map hashing per process (`SEEDED_ARMS` in
  `core/tools/std_contracts.py`), and map operations are core-source calls whose path depends
  on that key. Every entry that uses a `Map` therefore stays out of O5. O1 to O4 still judge
  it.
- **Tasks.** `determined()` in `core/tools/std_native.py` leaves out programs with tasks.
  Entries that spawn are kept out of O5 in the same way, and O1 to O4 judge them.

## Minimising

Determinism (SEM-8) means a red reproduces in one run on the build that showed it. The
**predicate** is "the same oracle fails, with the same signature, on the cheapest build that
reproduced it" (Cranelift dev before the LLVM builds, and natives at a small size before
large). The reduction runs in three stages.

1. **Choice-stream shrinking** (Hypothesis-style internal reduction). The reducer works on
   the choice stream:
   - it deletes blocks of draws;
   - it lowers draws toward 0, which always selects the simplest production;
   - it re-runs the generator.

   Every candidate comes out of the generator, so every candidate is well-typed and needs no
   `check` filter.
2. **AST passes**, each candidate filtered by `hirsel check`:
   - delete a statement, a scope or an entry;
   - replace an expression with a literal of its type;
   - unwrap a nested scope;
   - inline a function used once.
3. **Size bisection.** Binary search finds the smallest size and the largest `n` that still
   reproduce. For an E1-shaped red, this gives the threshold `L > n` directly.

Each red gets at most 300 predicate runs, or 10 CPU-minutes, whichever comes first, taken from
the night's reserve. When the cap stops an unfinished red, it resumes first the next night.

## Filing

The **dedup key** is `containment:<oracle>:<form>:<builds>`, for example
`containment:run-stop:clone-glue:cranelift-dev,llvm-dev,llvm-release` or
`containment:work-floor:structural-eq:all`. `<form>` is the production of the smallest
statement still present after minimisation. For each new key, the driver:

1. writes the minimised package, its argv, the oracle text and every build's output to
   `$run/containment/reds/<key>/`;
2. appends one line to the board's alert file (`$base/alerts/<date>.txt`), which
   `tools/nightly-board.sh` already routes;
3. files or updates one GitHub issue through `tools/ci/report_failure.py`. Today that script
   only works inside Actions: it requires the `GITHUB_*` variables and matches only issues
   opened by `github-actions[bot]`. The build adds a `--local` mode that matches the marker on
   issues opened by the `gh`-authenticated user and takes the run link from an argument. The
   existing marker format (`<!-- hirsel-ci-check: <key> -->`) stays, so a key that is already
   open gets a comment instead of a second issue.

Generator reds use the key `containment:generator:<refusal rule>`. The gap controls and the
O2 vacuity floor report in the run summary, not as issues. A lane that fixes a red adds the
minimised program to the containment law corpus (R6) as a law that is red on the parent.

## Nightly budget and hook

**Budget.** 1 CPU-hour a night, measured as the CPU seconds of the driver's child processes
(`getrusage(RUSAGE_CHILDREN)`).

- The driver stops starting new batches once spent time plus the running mean batch cost
  would pass 45 CPU-minutes. The last 15 minutes go to minimising.
- It never kills a running program to stay inside the budget. Each program run is capped at
  300 s, as `SEED_TIMEOUT_SECONDS` is in `tools/sim-campaign`, and a run that hits the cap
  is itself a red under the key `containment:timeout:<form>`.

**Cost per batch.** One batch is one package of M = 32 entries, compiled once per build.
Measured in slice C1 over seeds 0..200, eight batches at a time (wall seconds per part,
mean over the 200 batches):

| part | measured |
|---|---|
| evaluator: check, lowering and the 32 small-class entries | 1.23 s (max 1.82 s) |
| Cranelift dev compile | 0.35 s |
| Cranelift dev runs: 32 small-class entries and 8 large-class entries | 0.18 s (max 1.24 s) |
| whole batch, evaluator and Cranelift dev | 1.87 CPU-s |

The 200 batches took 374 CPU-s, and 9906 of 9906 compared records agreed exactly.

Measured in slice C2 over seeds 0..200 with every build and metering (`--meter-runtime`, the
testing runtime), eight batches at a time and so slower per part than C1 (wall seconds per
part, mean over the 200 batches):

| part | measured |
|---|---|
| evaluator: judged runs plus O5 metering of the metered entries (6 runs each) | 7.02 s |
| compile: Cranelift dev, LLVM dev, LLVM release | 1.18 s, 1.27 s, 3.35 s |
| judged runs, per native build | 0.17–0.23 s |
| metering: testing-runtime link and 6 runs per metered entry, per native build | 1.52 s, 1.73 s, 3.59 s |
| whole batch | 25.2 CPU-s |

The 200 batches took 5044 CPU-s, and 33156 of 33156 compared records agreed exactly, so
45 CPU-minutes cover about 107 batches, about 3400 programs.

**Hook.** In `tools/nightly-board.sh`, a `run_containment` step follows the
`core/tools/law_board.py nightly` call in the laws stage. It runs only when the toolchain
built, as the steps around it do. It:

- builds `//:hirsel-core-contract-eval-bin-hirsel-core-contract-eval` and the testing and
  charge-observation runtime archives that `std_native.py`'s metering already uses;
- runs `taskset -c 8-11 python3 core/tools/containment_fuzz.py nightly`, passing the compiler,
  the runtimes, `--cpu-seconds 3600`, the VOPR ledger and `--out "$run/containment"`;
- records `containment_status` through a new `--containment-status` auxiliary in
  `tools/board-freshness.py`, next to `--law-status`.

A containment red never changes the board's own exit status. The hook is described here
only: the edits to the script and to `board-freshness.py` belong to slice C4, and the systemd
units (`tools/hirsel-board*.service`, `.timer`) stay as they are.

## Reuse, not parallel machinery

| need | existing piece | change |
|---|---|---|
| build and run one entry on each engine | the per-case path inside `run()` in `core/tools/std_native.py` (manifest, wrapper, resident evaluator, native link and invoke) | move it into a new module, `core/tools/contract_exec.py`, which the contract walker and the fuzz driver both call. `std_native.py` is at the 1000-line cap of `tools/check_file_lengths.py`, so it must shrink, not grow. `discover()` stays tied to the repo's test directories, so the driver builds its `Contract` rows itself |
| step floor, per-build repeat, injected refusal, native ≤ evaluator bytes | `check_metering`, `evaluator_meter` and `native_meter` in `core/tools/std_contracts.py` | none |
| a row per build pair | `engines_agree` in `std_native.py` | adopt its row format for O2 |
| the evaluator's report and meter | `Report.steps`, `step_floor` and `step_refusal`, and `StepMeter`, in `crates/hirsel-core-eval` | add the O3b tally beside `take_work` in `metering.rs`, behind `HIRSEL_TEST_WORK_TALLY` |
| native step report and ceiling | `hirsel_core_test_step_report` and `HIRSEL_TEST_WORK_CEILING` in `crates/hirsel-core-rt/src/steps.rs` | none |
| seed ledger, replaying earlier reds first, the timeout cap | `tools/sim-campaign` | allocate under the campaign name `containment-gen` |
| issue dedup | `tools/ci/report_failure.py` | add a `--local` mode |
| real memory against charge | `crates/hirsel-core-rt/src/peak.rs` | none |

The Core IR differential in `crates/hirsel-cranelift/tests/support/providers.rs` is **not**
the place to generate programs. Clone, `==` and drop glue are made by the front
(`crates/hirsel-core-front/src/body/clone_glue.rs`, `key_glue.rs`), so programs generated
below the front could never reach E1 or E2.

## Prior art

| system | what it does | verdict |
|---|---|---|
| wasm-smith (bytecodealliance/wasm-tools) | generates valid Wasm by checking a validity predicate per instruction against the operand and control stacks; choices come from `arbitrary::Unstructured` bytes | **ADOPTED:** a validity predicate per production, over the typing environment. **ADAPTED:** the byte source becomes a SplitMix64 choice stream with explicit size classes; driven by bytes, wasm-smith seldom produces large values (its `max_instructions` defaults to 100). **NOT TAKEN:** `ensure_termination`, which injects a fuel global; for us the bound is the thing under test, not a harness guard |
| wasm-mutate and wasm-shrink | semantics-preserving mutation; a shrinker driven by an `is_interesting` predicate, with an attempts cap of 1000 | **ADOPTED:** the predicate plus an attempts cap. **NOT TAKEN for now:** mutating the existing law corpus as extra seeds; a later slice could add it |
| Wasmtime's differential fuzzing (`crates/fuzzing/src/oracles.rs`) | compares engines, and treats `StackOverflow` and allocation traps as nondeterministic (`wasmtime_trap_is_non_deterministic`) | **ADAPTED:** the O2 relation also tolerates the stack and capacity results, but Capacity only in MEM-28's direction, and **never** a run stop: FAIL-15 and R2 forbid one in a contained scope, which is stricter than Wasmtime. Wasmtime uses fuel only as a timeout; here steps are an oracle |
| Csmith | avoids undefined behaviour by construction with `safe_*` wrappers, and outputs a CRC of the globals as its observable | **ADOPTED:** a compact per-scope observable, the tag stream. **REPLACED:** the safe wrappers become the spec-derived tolerance relation, because Hirsel has no UB but does have divergence each build may legally show. Csmith programs are large and need C-Reduce; here programs stay small, and reduction runs on the choice stream |
| YARPGen | generation policies; evaluates expressions while generating and rebuilds any with UB | **ADOPTED:** policies per batch (swarm testing). **ADAPTED:** the generator computes `L` and the logical bytes during generation, as YARPGen computes values, and emits them as self-checks |
| Goose `bench/bce_fuzz.py` (peer) | shape templates for the loops its bounds-check pass targets, with off-by-one mutations; the JIT runs each with the pass on and off, and the runs must agree | **ADAPTED:** targeted shapes (big input outside, tiny bound inside) inside a typed generator. **NOT TAKEN:** untyped string templates and `random.Random`, which give neither a well-typedness guarantee nor stable replay, and its lack of a reducer |
| Hypothesis | test-case reduction on the choice sequence | **ADOPTED:** every shrunk candidate is a generator output, so it is well-typed |

## Build slices (Sol lane)

Each gate is one test, run once. A gate either shows a red caused by a real open gap or shows
the new check working on real programs. There are no seeded bugs and no mutants.

| slice | scope | gate | reds that may ride |
|---|---|---|---|
| **C1, vertical thread** | `containment_gen.py`: SplitMix64 stream, typed environment, the template with `clone`, `==`, loops, recursion and nested `isolate` / `bound steps` at both size classes, O3a self-checks, tag stream. `containment_fuzz.py`: one batch through the factored `std_native` path on evaluator and Cranelift dev, with O1, O2 and O3a. Measure the real batch cost and replace the estimates above | `containment_fuzz.py once --seed <s> --builds evaluator,cranelift-dev` at the large size class finds the expected O1 red: a deep `Box` chain cloned with `n` above depth/4 and depth past the stack (E2, while 3227 is open). If 3227 has landed, the gate is the measured cost table plus a green batch with ≥ 80% of records compared exactly | O2 differences in the tolerated rows; any E2-class O1 red still open after 3227 |
| **C2, all builds and metering** | LLVM dev and LLVM release; O5 through `check_metering`; O4a, O4b and O4c; the Map and task exclusions | `containment_fuzz.py once` over the three native builds and 32 seeds: green, or a red filed under its key, with ≥ 80% of records compared exactly | none of the compile, schema or policy reds |
| **C3, independent work tally** | the O3b evaluator tally behind `HIRSEL_TEST_WORK_TALLY`; the O3c static cycle check; gap controls | one batch with the tally on: every gap control shows its sanctioned O3b excess (the named MEM-30 gaps are open on main), and any step-free non-drop glue cycle O3c finds is filed under its key | gap controls are reported, not red |
| **C4, minimise, file, nightly** | choice-stream and AST reduction, size bisection; dedup keys; `report_failure.py --local`; ledger allocation; O4d on the charge-observation runtime; the `run_containment` step in `tools/nightly-board.sh` and `--containment-status` in `tools/board-freshness.py` | one `containment_fuzz.py nightly --cpu-seconds 300` run minimises the open C1 red to at most 20 lines and writes exactly one alert line and one issue marker for its key; run twice, it adds one comment, not a second issue | the night's containment reds are findings and never fail the land |

C1 to C3 touch only `core/tools/` and `crates/hirsel-core-eval` (behind `HIRSEL_TEST_WORK_TALLY`). C4 is the
first slice to touch the board script. The board's units are not edited.
