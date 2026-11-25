# Coverage

`hirsel test --coverage` reports which function entries and source control-flow arms the
selected tests reached. `hirsel coverage` merges, reports and diffs saved profiles without
running anything.

Version note (2026-11-25, coverage-c0-contract-2547): first text. This document is the tool
contract for coverage slice C0. It specifies Sam's rulings of 2026-11-25 and the lead's
reversible defaults of the same day:

- **D1.** Coverage is its own observation switch beside recording, off by default
  ([CAP-9](spec/capabilities.md#cap-9-recording-is-outside-the-program),
  [SEM-8](spec/semantics.md#sem-8-determinism)).
- **D2.** Coverage runs under `hirsel test --coverage`, alone or with `--laws`. Saved
  profiles are merged and diffed by `hirsel coverage`. `hirsel laws` stays static and about
  the lock.
- **D3.** A site is a one-byte reached flag, not a hit counter.
- **D4.** Sites are function entries and source control-flow arms. Failure edges are not
  sites.
- **D5.** Only packages given as source have sites.
- **D6.** The raw profile is a file mapped over the flags at process entry.

D3 to D6 are reversible until slice C4 publishes the profile format.

Slices C1 to C5 build this in order, and each section below says which slice owns it.

Version note (2026-11-25, coverage-c1-cranelift-2551): slice C1 built the sites, the switch,
the Cranelift flag stores, the raw file and the map fragments. It amended "Sites" with what
the store sample showed (the site rule is not yet accepted), the header's section offset,
the id framing, the outcome, the entry's padding record, fragment naming, and "Caching".

Version note (2026-11-25, coverage-gen-anchors-2579): the front marks source decisions, and
only they are sites. The terminators it generates inside an expression keep their anchors
but no longer count. "Sites" states the final rule. Site keys keep their format.

These are out of scope:

- hit counts, and any reading of coverage by the program it observes;
- failure edges of instructions and calls as sites;
- a minimum-coverage option or any other gate;
- profile-guided optimisation from these files;
- separate profiles for the cases of one property run;
- `hirsel build --coverage` and `hirsel run --coverage`;
- coverage from the evaluator.

This document, like [recording](recording.md) and the [law table](laws.md), is a tool
contract and not part of the language. Under R28 ([reserved](spec/reserved.md)), no profile,
flag or report is an input to acceptance, lowering, code identity, replay or any observable
of [SEM-8](spec/semantics.md#sem-8-determinism), and no program reads one. No exit code
depends on coverage, and no profile is ever fed back into a build. Under R29 nothing here
adds a cause kind.

**Not a gate.** Coverage shows where to look. It is never a merge condition, and never a
reason to keep or drop a checked-in test.

## The switch

Coverage is a build switch, off by default and independent of every other build choice:
engine, target, optimisation level, recording, and `--debug-lines`. A **coverage build** is
the build without it plus a table of reached flags and one byte store at each site. It is
the same build in SEM-8's sense: a run of it makes the same observations as a run of the
build without the switch, with the program given the same values at the replay point. That
includes its steps, slice ends, charges and `Capacity` refusals.

**What stays the same.**

- **The checked IR.** The switch is a field of the CoreIR module that is never serialized,
  like law observation. It adds no instruction, block or edge.
- **Identity.** Every body hash, annex, group hash, address, test address and
  `hirsel_code_revision` stays the same bytes
  ([PROG-5](spec/programs.md#prog-5-code-identity), PROG-9, PROG-10).
- **Steps.** The step plans read CoreIR, so a coverage build debits the same steps at the
  same points.
- **Charges.** The flag table is static storage outside every limit. No allocation is added
  or removed.

**The store.** A flag store writes the constant 1 into a table that no CoreIR instruction
can address. It obeys CAP-9's rules for a recording:

- it charges no limit, takes no step, and adds no event, poll or provider operation;
- it changes no delivery;
- nothing it writes is built in program space;
- when the profile file cannot be written, the run goes on unchanged and its coverage is
  `unobserved`.

An optimiser may move a flag store, or inline differently because of it. It may not set a
flag on a path whose edge did not run. That changes speed and code size, never an
observable. If a metering pass changes a result because of an unrelated store, the
"Same observations" law goes red, and the fix belongs in that pass.

**Zero cost when off.** A build without the switch computes no site table, emits no store,
has no `hirsel_cov` section and calls no coverage runtime function. Its objects are the
bytes, and its object, release-part and executable keys the keys, it has without the switch.
The coverage runtime functions are called only from a coverage executable's entry. A link
that collects unused sections drops them, and a dev link keeps them uncalled, as it keeps
every unused runtime function. The existing runtime archives serve both builds and no
archive is added.

**Program-invisible.** No language or std operation reads a flag, and the program cannot
ask whether coverage is on. The runtime removes `HIRSEL_COVERAGE` from the environment
before user code runs, as it does `HIRSEL_LAWS`. A child process the test starts does not
inherit it.

Coverage is separate from the recording build. A `--laws` run has no flags unless
`--coverage` is also given, and a `--coverage` run is not a recording build unless `--laws`
or `--replay` also asks for one. Both switches may be on together.

## Sites

A **site** is one place whose flag a run can set. A body has two kinds of site.

- **Entry.** One site per in-scope function body, set when the body starts.
- **Arm.** One site per outgoing edge of a source decision. A two-way branch has the arms
  `yes` and `no`. A switch has one arm per case, numbered from 0 in terminator order, and
  `otherwise`.

**Source decisions.** The front marks a block's terminator as a source decision
(`Block::decision` in CoreIR) only when the source wrote it:

- the test of an `if`, `else if` or `while` condition, at the condition;
- a `for` loop's head test, at the whole loop, and for an inclusive range `a..=b` also its
  end test, which is the only way out of a loop that ran; it is the loop's decision `.1`;
- a `?`, at the `?` expression;
- each test of a refutable pattern in a `match`, `if let`, `while let` or `let ... else`,
  at the pattern, and a `match` guard, at the guard;
- the left side of `&&` and `||`, whose arms are short-circuit and evaluate the right side.
  The construct that reads the result decides again at the whole expression.

A decision is always an anchored branch or switch. No source construct lowers to a switch
today, so every arm is `yes` or `no`.

**Not sites:**

- terminators the compiler generated: glue with no anchor, and the branches the front
  generates inside a source expression, for example in built-in calls (`list.push`,
  `list.try_reserve`, `int.checked_add`, `int.narrow`), indexing and field reads, `==` on
  non-scalar values, and capacity and limit admissions. These keep their expression's
  anchor for debug lines and law rows, but are not decisions;
- failure edges of steps, address checks, drops, SIMD, arithmetic and calls.

A failure the source handles appears as an arm of the `match` or `?` that handles it. A
failure the source does not handle has no arm to cover.

**Arms are edges.** An arm's flag is set when its edge is taken, not when its target block
starts. For an `if` with no `else`, the `no` edge goes to the join block that both paths
reach, so only an edge flag can tell whether `no` was ever taken.

**One definition of order.** One function, `coverage::sites` in `hirsel-core-ir`, returns a
body's sites in a fixed order. Both engines and the driver call it, so the order is the same
on every engine and every optimisation level.

**Site keys.** A key is stable text:

| kind | key |
|---|---|
| entry | `PATH#e` |
| arm | `PATH@START-END.N:ARM` |

- `PATH` is the full definition path of the source item that holds the site. For an entry,
  it is the function's definition. For an arm, it is the anchor's item, which is how the
  law observer names a site.
- `START` and `END` are the anchor's byte offsets, relative to the start of the item.
- `N` numbers the decisions that share one anchor, from 0, in `coverage::sites` order.
- `ARM` is `yes`, `no`, a decimal case number, or `otherwise`.

Example: `store@1/tiered/compact@1180-1213.0:no`.

A key belongs to the source item, not to an instance. Every instance of a generic function
and every code variant of a body (the step-free and returnable copies) writes the same
sites. Offsets are relative to the item, so an edit elsewhere in the file leaves the keys
unchanged.

**The rule holds on `store`.** The site table of the `store` package, over the bodies its
`tiered` tests reach, had 13,215 decisions in 1,571 bodies while the front gave each generated terminator its
expression's anchor and every anchored branch was a decision. With the decision mark it has
7,222 decisions in the same 1,571 bodies, and every one of them is also a decision of the
earlier table, at the same anchor, arm and target. The 5,993 dropped decisions are generated:
scope cleanup on whole blocks, `list.try_reserve`, `list.push` and other builtin calls,
indexing, host calls, the error path of `?` and the inner tests of `for` and of `==` on
lists. In a random sample of the decisions that remain, each is an `if` or `while`
condition, a `for` head, a `?` or `?=>`, a `match` or `if let` pattern, or `&&`/`||`. In
322 keys the ordinal now names the source decision rather than a generated test that
shared its anchor (for example `if key_schema != c.key` and its list compare).

One key can name decisions in more than one code variant of a body (the step-free and
returnable copies). Their sites are equal, and only the first anchored place of an arm's
target can differ.

**Scope.** Only items of packages given as source in the request have sites: the workspace
members, plus `core` and `std` when they are given as source (`--source-core`,
`--source-std`, `--libraries`). Published dependencies and the installed std product have
no sites. To cover `std.sim`, run std's own tests with std given as source.

**The denominator.** The total is the sites of every in-scope body that the selected tests
reach in the build. A function no selected test references is never lowered and has no
sites. It is listed by name as **unreferenced**, from the checked product's item list, so it
stays visible. Sites are retained against section garbage collection and the LLVM global
optimiser, so a function inlined everywhere, or deleted as dead, still counts in the total.

**Foreign code is unobserved.** A foreign function has no CoreIR body and no sites. A test
that reaches foreign code still gets a profile of its Hirsel code. Unlike recording, coverage
does not refuse a program that reaches foreign code
([FFI-8](spec/foreign.md#ffi-8-a-program-that-reaches-foreign-code-is-never-recorded-replayed-or-reused)).

**Child processes are unobserved.** Code a test runs in another process has no profile, and
its sites read as not reached. A tool that wants their coverage builds the launched
executable with `--coverage` and sets `HIRSEL_COVERAGE` itself.

## Reached

A site is **reached** in a run when its flag was set at any point in the run. The flag is
set when the edge runs or the body starts, and is never cleared. It is not a count, so a
site reached once and a site reached a million times read the same.

- **Concurrency.** A flag is idempotent, so workers in one process cannot lose an update.
- **Determinism.** A run's flags are a function of the run. A determined run on one build
  gives the same profile every time.
- **Engines.** Flags are a function of the program and its inputs, not of the engine. The
  Cranelift and LLVM profiles of one determined run are equal; the "Engines agree" law
  checks this.
- **Dead arms.** An arm the optimiser proves dead keeps its flag at zero, which is the truth.

Frequency comes from merging. Each test process is one **run**: one test at one seed, on one
build. A profile counts how many runs reached each site.

## Commands

| form | does | slice |
|---|---|---|
| `hirsel test PKG --coverage [--json]` | builds the coverage build, runs the selected tests, reports | C2 |
| `--coverage-out FILE` | also writes the merged profile to FILE | C2 |
| `--coverage-base FILE` | adds a diff against an earlier profile | C4 |
| `--coverage-targets FILE` | restricts the uncovered list to source ranges | C4 |
| `--coverage-runs` | keeps one row per run in the profile | C4 |
| `--seed-base K` | with `--seeds N`, runs seeds K to K+N-1 | C4 |
| `hirsel coverage FILE... [--map FILE]... [--base FILE] [--targets FILE] [--json] [-o FILE]` | merges, reports and diffs offline | C4 |

**`--coverage`** composes with `--release`, `--backend`, `--target`, `--sim`, `--seed`,
`--seeds`, `--filter`, `--exact`, `--laws`, `--laws-baseline`, `--replay`,
`--property-corpus`, `--rerun`, `--test-timeout` and `-j`. A narrowed run is allowed, unlike
under `--laws-baseline`, because coverage is not a verdict. The profile records what was
selected. `--coverage-out`, `--coverage-base`, `--coverage-targets` and `--coverage-runs`
each imply `--coverage`.

**`--laws --coverage`** builds the recording build with coverage. Each test prints its law
rows and its coverage row, and `--json` carries both. The two observations are separate.
Coverage never enters a law row, `law_strength`, `hirsel.laws` or a baseline, and never
creates a `sometimes`.

**`--replay --coverage`.** The profile is taken from the recorded run. The replay writes
none.

**`--coverage-targets FILE`** reads one JSON object on each line:
`{"file": "tiered.hirsel", "begin_line": 200, "end_line": 260}`. Lines are 1-based and both
ends are inclusive. `file` is matched against the site's file path relative to its package
root, or by its final path component when the target names no directory. A site is
**targeted** when its decision's source range overlaps a target. Extra fields are ignored,
so a target list written for another coverage-guided tool in this format is read as it is.
Targets narrow the uncovered list and mark the targeted rows. Totals are unchanged.

**`--seed-base K`** requires `--seeds` and conflicts with `--seed`. It exists so that a
fresh seed range can be checked against a fixed one (see the recipe).

**`hirsel laws`** keeps only `check` and `write`. It runs no test and has no coverage
option.

**Exit codes.** `hirsel test` exits as it does without coverage. A coverage failure never
changes the status: an unwritable profile file makes the run `unobserved` and is reported.
`hirsel coverage` exits 0 when it reports, 1 when an input is refused (unreadable,
conflicting, or naming an id no map holds), and 2 on usage errors.

## Files

### The site map (`.covmap`)

With `--coverage` and `-o DIR`, `hirsel test` writes each test's executable to `DIR/HEX`,
where `HEX` is the content hash of the test's definition path, as it does today. Beside it
the test writes `DIR/HEX.covmap`, the site map of that executable. A raw file is read
against the map of the executable that wrote it.

A map is one JSON document:

```json
{"hirsel_covmap": 1,
 "test": "store@1/tiered_random_laws/law_tiers",
 "build": {"engine": "llvm", "optimize": true, "target": "x86_64-unknown-linux-gnu"},
 "scope": ["store@1"],
 "functions": [{"definition": "store@1/tiered/compact", "src": "1f0c3a9e5b7d2c41",
                "file": "tiered.hirsel", "line": 210, "col": 1}],
 "records": [{"id": "<64 hex>", "sites": [
   {"site": "store@1/tiered/compact#e", "kind": "entry", "definition": "store@1/tiered/compact",
    "file": "tiered.hirsel", "line": 210, "col": 1},
   {"site": "store@1/tiered/compact@1180-1213.0:no", "kind": "arm", "arm": "no",
    "definition": "store@1/tiered/compact", "file": "tiered.hirsel",
    "line": 244, "col": 9, "end_line": 244, "end_col": 42, "arm_line": 251, "arm_col": 13}]}],
 "unreferenced": [{"definition": "store@1/tiered/debug_dump", "file": "tiered.hirsel", "line": 12}]}
```

- `records` lists each id once, with its sites in `coverage::sites` order. Flag `i` of a raw
  record is site `i` of its id.
- `file` is relative to its package root. Lines and columns are 1-based. Columns count
  Unicode scalar values.
- `line`/`col` to `end_line`/`end_col` is the decision's source range, from its anchor.
- `arm_line`/`arm_col` is the first anchored position in the arm's target block. It is
  absent when the arm is empty, as for the `no` arm of an `if` with no `else`.
- `src` is the first 8 bytes, in hex, of SHA-256 over the function's source bytes without
  doc comments, framed by its length in eight little-endian bytes, as in `hirsel.laws`.

The driver builds the map at link by joining the map fragment stored with each linked
object. A fragment is named by the object's identity, under the domain
`hirsel.core-object-coverage.v1`, and is written before the object's root record. Positions
come from the native source table, as law rows do.

### The raw file

The runtime writes one raw file for each process whose environment names one. The format is
the interface between a coverage executable and every reader, including
`core/tools/sim_campaign.py`, which runs executables directly.

**Startup.** A coverage executable's entry calls `hirsel_core_coverage_open(start, length)`
before user code and before any thread starts. The function reads `HIRSEL_COVERAGE` and
removes it from the environment. If the variable named a path, the function:

1. creates the file, `create_new`, with mode 0600;
2. writes the header page;
3. copies the pages that hold the `hirsel_cov` section after it;
4. maps the file over those pages, shared and fixed.

From then on every flag store lands in the file. There is no exit hook and no flush. A
passing run, a failing run, a run stopped by a bound, an abort, and a run the runner kills
at its timeout all leave every flag they set. If any step fails, the section stays ordinary
memory, the run goes on unchanged, and the file is absent or has no `end` mark (below), so
the run reads as `unobserved`.

**Header.** The first page, at the target's page size. All integers are little-endian.

| offset | size | field |
|---|---|---|
| 0 | 8 | magic, the bytes `HIRSELCV` |
| 8 | 4 | format version, 1 |
| 12 | 4 | page size |
| 16 | 8 | offset of the section in the file: one page plus the section's offset in its first page |
| 24 | 8 | length of the section |
| 32 | 4 | seed present: 1 when `HIRSEL_TEST_SIM_SEED` parsed as a u64 at startup, else 0 |
| 36 | 4 | outcome: 0 not recorded, 1 passed, 2 failed |
| 40 | 8 | the seed, or 0 |
| 48 | 4 | end mark, `0x45444F4E`, written last at startup |

Reading the seed is not an effect of the program: the runtime reads the variable as given and
leaves it in place. The outcome is written once, by the entry code, from the exit status the
entry returns: 1 when it is 0, else 2. A run killed or aborted before that, or one that ends
by calling `exit` directly, leaves outcome 0. A reader counts outcome 0 as failed, because
PROG-4 fails a test that a stop ends.

**Records.** The section copy is a sequence of records, each starting at an 8-byte-aligned
offset:

| field | size |
|---|---|
| magic `0x56434948` | 4 |
| `n`, the number of sites | 4 |
| `id` | 32 |
| flags, one byte per site, 0 or 1 | `n` |
| zero padding to the next 8-byte boundary | 0 to 7 |

Any zero bytes between records are padding, and a reader skips them in 8-byte steps. The
entry adds one 8-byte zero record, so the section exists in every coverage executable. `id`
is SHA-256 over the domain tag `hirsel.coverage.sites.v1` and then each site key in order,
each of them, the tag included, framed by its length in eight little-endian bytes. Two bodies with the same sites share
an id. Their flags are OR-ed when the file is read. A raw record whose id no given map holds
is an error naming the file, never a silent drop.

**Raw file to run.** A raw file is one run. Its test comes from the map it was read against,
its seed and outcome from the header, and its build from the map. Under `hirsel test` the
runner knows all four and records them itself.

### The profile

`--coverage-out` and `hirsel coverage -o` write a profile, one JSON document:

```json
{"hirsel_coverage": 1,
 "builds": [{"engine": "llvm", "optimize": true, "target": "x86_64-unknown-linux-gnu"}],
 "scope": ["store@1"],
 "selection": {"tests": 147, "runs": 4704, "digest": "<16 hex>"},
 "runs": 4704, "failed_runs": 3, "unobserved_runs": 0,
 "functions": [{"definition": "store@1/tiered/compact", "src": "1f0c3a9e5b7d2c41",
                "file": "tiered.hirsel", "line": 210, "col": 1}],
 "sites": [{"site": "store@1/tiered/compact#e", "kind": "entry",
            "definition": "store@1/tiered/compact", "file": "tiered.hirsel", "line": 210,
            "col": 1, "passed_runs": 96, "failed_runs": 1}],
 "unreferenced": [{"definition": "store@1/tiered/debug_dump", "file": "tiered.hirsel", "line": 12}],
 "tests": [{"definition": "store@1/tiered_random_laws/law_tiers", "seeds": [[0, 31]],
            "runs": 32, "failed_runs": 0, "reached": "<base64>"}],
 "run_rows": [{"definition": "store@1/tiered_random_laws/law_tiers", "seed": 3,
               "passed": true, "reached": "<base64>"}]}
```

- **`sites`** is sorted bytewise by key. A site row carries the map's fields for its key,
  plus `passed_runs` and `failed_runs`, the number of observed runs that reached it. Those
  two counts are enough for a quality score or an Ochiai ranking without per-run rows.
- **`reached`** is a bitset over `sites`. Bit `i` is site `i` of the `sites` array. It is
  stored in byte `i / 8`, at bit `i % 8`, counting from the least significant bit. The bytes
  number `ceil(sites / 8)`, with unused high bits zero, and are written in standard base64
  with padding.
- **`seeds`** lists the seeds of a test's runs as sorted, merged, inclusive ranges.
  `null` stands for the one run of a test without `--sim`.
- **`run_rows`** is present only with `--coverage-runs`. Without it, runs are folded by test.
- **`selection.digest`** is the first 8 bytes, in hex, of SHA-256 over the sorted
  `definition`/seed pairs of every run, each framed by its length.
- **`unobserved_runs`** counts runs whose raw file was missing or unfinished. They are in no
  other count.

**Merge.** Sites match by key. Counts add, bitsets are OR-ed, and seed ranges join. Inputs
are refused, naming the conflict, when:

- **Same run twice.** Two inputs hold the same run (test, seed, engine and optimisation
  level). A run cannot be taken back out of a count.
- **Different sources.** Two inputs give different `src` for one function.

Profiles from different engines merge, because keys do not depend on the engine. `builds`
then lists each build.

## Report

### Fields

Every report field derives from the folded profile. None needs `run_rows`.

- **Reached / total.** For functions, a function is reached when its entry site is. For
  arms, the count is of arm sites. `unreferenced` is the length of that list.
- **`entered_runs`** of a function is its entry site's `passed_runs + failed_runs`.
- **Uncovered states.** Each site that no run reached is in exactly one state:

  | state | when | listed as |
  |---|---|---|
  | `never entered` | the function's entry site was not reached | the function, once, with its count of arms |
  | `other arm taken` | the function was entered and a sibling arm of the same decision was reached | the arm, with `entered_runs` |
  | `decision not reached` | the function was entered and no arm of the decision was reached | the arm, with `entered_runs` |

  The arms of a function that was never entered are not listed one by one.
- **`only`**, per test, is the number of sites this test reached and no other test in the
  profile reached. It is computed from the per-test bitsets. It reads "a test is worth the
  bug it alone catches" as coverage.
- **`target`** is true on a site that `--coverage-targets` selected, and absent otherwise.

### Text

```text
coverage  store@1  llvm release  147 tests, 4704 runs
  functions 801/933   arms 5120/7410   12 unreferenced
  tiered.hirsel            functions 41/44   arms 310/402
  bucket.hirsel            functions 63/63   arms 502/611
never entered (3 of 132; --json lists all)
  store@1/tiered/rewrite_tail          tiered.hirsel:412
entered, arm not reached (most-entered first)
  store@1/tiered/compact   tiered.hirsel:244:9   no    other arm taken        entered by 96 runs
  store@1/tiered/plan      tiered.hirsel:301:5   2     decision not reached   entered by 40 runs
since base  +119 arms, -0   2 functions changed
```

The text report lists at most 20 rows in each section, then the count left. Rows sort by
`entered_runs`, largest first, then by key. A test row of `hirsel test --coverage` gains its
counts after the outcome: `coverage 212 fn, 1490 arms, 7 only`.

### `--json`

Each row of `tests` gains a `coverage` object, and the document gains a top-level
`coverage` object:

```json
{
  "tests": [
    {"definition": "store@1/tiered_random_laws/law_tiers", "passed": true, "source": "executed",
     "coverage": {"functions": 212, "arms": 1490, "only": 7}}
  ],
  "coverage": {
    "schema": 1,
    "builds": [{"engine": "llvm", "optimize": true, "target": "x86_64-unknown-linux-gnu"}],
    "scope": ["store@1"],
    "selection": {"tests": 147, "runs": 4704, "digest": "<16 hex>"},
    "runs": 4704, "failed_runs": 3, "unobserved_runs": 0,
    "totals": {"functions": {"reached": 801, "total": 933},
               "arms": {"reached": 5120, "total": 7410}, "unreferenced": 12},
    "files": [
      {"file": "tiered.hirsel",
       "functions": {"reached": 41, "total": 44}, "arms": {"reached": 310, "total": 402}}
    ],
    "functions": [
      {"definition": "store@1/tiered/compact", "file": "tiered.hirsel", "line": 210,
       "src": "1f0c3a9e5b7d2c41", "entered_runs": 96, "arms": {"reached": 14, "total": 22}}
    ],
    "uncovered": [
      {"site": "store@1/tiered/rewrite_tail#e", "kind": "entry", "state": "never entered",
       "file": "tiered.hirsel", "line": 412, "col": 1, "arms": 9},
      {"site": "store@1/tiered/compact@1180-1213.0:no", "kind": "arm",
       "state": "other arm taken", "arm": "no", "entered_runs": 96,
       "file": "tiered.hirsel", "line": 244, "col": 9, "end_line": 244, "end_col": 42,
       "arm_line": 251, "arm_col": 13, "target": true}
    ],
    "unreferenced": [{"definition": "store@1/tiered/debug_dump", "file": "tiered.hirsel", "line": 12}],
    "diff": {"base": "<16 hex>", "selection_changed": false,
             "reached": {"before": 5001, "after": 5120},
             "gained": ["<key>"], "lost": [], "changed_functions": ["store@1/tiered/plan"]}
  }
}
```

`uncovered` lists every uncovered row with no limit. With `--laws`, each test row also
carries `laws` and `law_strength` as the [law table](laws.md#output) says. `hirsel coverage
--json` prints the `coverage` object alone.

### Diff

`--coverage-base FILE` and `hirsel coverage --base FILE` compare by site key.

- **`gained`** and **`lost`** list the keys reached in one profile and not the other.
- **`changed_functions`** lists each function whose `src` differs between the two. Its sites
  are left out of `gained`, `lost` and both `reached` counts, so an edit to the code under
  test is not reported as a change in coverage.
- **`base`** is the base's `selection.digest`, and **`selection_changed`** is true when the
  digests differ. The diff still prints, and the text report says `selection differs`.

## Recipe: checking a generator edit

A generator edit is worth keeping when it raises the reached count on a fixed seed set and
on a fresh one. Coverage is a guide here, never a keep criterion for a checked-in test.

**Sim seeds.** `--seeds N` runs seeds 0 to N-1. `--seed-base K` moves the range to K to
K+N-1, so the fresh set does not overlap the fixed one.

**Data seeds.** A `std.prop` property draws case `i` from `std.rand.new(c.seed, i)`. Its
data depends on `Config.seed` alone. `Config.sim_seed` moves only the worlds of task
properties ([prop](../core/std/api/prop.md#sim-seeds)). The runner grants the sim seed
through `Env` as `HIRSEL_TEST_SIM_SEED`. It cannot reach a `seed` written as a literal in the
test. So a property whose data the seed range should move reads the granted seed into
`seed`:

```hirsel unbuilt
@test
fn law_tiers_swept(e: Env, tasks: Tasks) -> Result[(), ()] waits {
    let mut c = std.prop.config()
    let s = std.sim.world.test_seed(e)
    c.seed = match s { Some(n) => n, None => 0 }
    c.sim_seed = s
    std.prop.check_tasks(tasks, c, tier_ops(), law_tiers)
}
```

- The test's grants must include `Env=HIRSEL_TEST_SIM_SEED`, in `hirsel.toml` or by
  `--grant`.
- A property without tasks sets `c.seed` only.
- A test that takes `Env` is not parameterless, so its pass is never reused. Each seed is a
  fresh run.
- Without `--sim`, `test_seed` returns `None`, and the property keeps `seed = 0` as before.

Then:

```sh
hirsel test PKG --sim --seeds 32 --coverage-out before-fixed.json
hirsel test PKG --sim --seeds 32 --seed-base 1000000 --coverage-out before-fresh.json
# edit the generator
hirsel test PKG --sim --seeds 32 --coverage-base before-fixed.json
hirsel test PKG --sim --seeds 32 --seed-base 1000000 --coverage-base before-fresh.json
```

Keep the edit when both diffs show `reached` rising and no function in `changed_functions`
other than the generator's own. One process is one run, so all of a property's cases count
together.

## Caching

- **Options.** `NativeOptions` gains `coverage`, the scope or none. The places that set the
  module's law-observation field set the coverage field from it.
- **Keys.** The native environment tuple gains the coverage scope, appended only when
  coverage is on. Object, release-part and executable keys already hash that tuple, so
  coverage objects and plain objects never collide, and they share one store. The request
  record's native tuple appends it the same way. The incremental link frame does not take
  it, so a coverage build links from scratch.
- **The edit loop is untouched.** A plain build's tuple has the bytes it has without the
  switch. Nothing it looks up or writes changes, and a `--coverage` run evicts nothing.
- **Installed std.** Std is out of scope unless given as source, so the release std's key
  does not take the field. A coverage run over an installed std compiles only the program.
- **Map fragments** are stored with each object, named by its identity.
- **Test passes.** The pass cache schema becomes 5. A cached pass gains its raw profile. A
  `--coverage` run reuses a pass only when the pass was recorded with its profile, the rule
  `--laws` uses for rows. This is sound because a flag profile is a function of a determined
  run, and only parameterless tests are cached. Runs under `--sim`, `--replay` or
  `--property-corpus` are never cached, with or without coverage.
- **C1 confirmed** that the test key's build-record digest covers the environment tuple,
  and that the resident compiler's memo is keyed by it. A plain pass is not served to a
  coverage run.

## Laws

Each law names the bug it catches, the harness it joins and its oracle. Cases go in the
named existing harnesses. None adds a new harness.

| law | bug it catches | harness | oracle |
|---|---|---|---|
| **Same observations** | a flag store moves a step-bound stop, a slice end, a `Capacity` value or output | the recording same-observation harness in `crates/hirsel-core-driver/src/recording_build_native.rs`, as one more build per engine and optimisation level. Its body prints what depends on the stop point: the iterations completed by a loop with a call in it before `bound steps n` stopped it, `Capacity.available` at a refusal after several admissions, and the output order of two tasks that each cross a slice end | the build without coverage |
| **Tape crosses** | the coverage build schedules differently | record the two-task body above on the plain recording build, and replay it on the coverage recording build; it must end `Sealed`, not `Diverged` | the recorded tape |
| **Identity** | the switch leaks into serialized IR or a revision | `crates/hirsel-core-driver/src/revision_native.rs` | `hirsel_code_revision` and test addresses of the plain build |
| **Engines agree** | a missed block copy on LLVM, or the engines ordering sites differently | the contract corpus run by `core/tools/std_native.py`: for each determined law, the Cranelift and LLVM release profiles are equal | the other engine |
| **Known arms** | an arm index off by one, or an `if` without `else` reported from its join block | one package in `crates/hirsel-core-driver/src/test_run_native.rs` with an `if` and no `else`, a three-arm `match` with one arm unreachable, and `&&` | keys derived by hand |
| **Killed run** | a profile lost when the runner kills a test | the timeout case in `crates/hirsel-core-driver/src/test_run_native.rs` | the site before the hang is reached, and the run counts as failed |
| **Off is off** | a coverage object reused by a plain build | `crates/hirsel-core-driver/src/test_run_native.rs`: after a `--coverage` run, the plain executable has no `hirsel_cov` section, and a plain pass is not served as a coverage pass | the section's absence |

"Same observations", "Tape crosses" and "Engines agree" on LLVM release belong to slice C3.
A red in any of them blocks a landing, since it means the coverage build is a different
build.
