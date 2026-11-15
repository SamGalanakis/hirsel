# Contributing to Hirsel

This page covers building, testing and landing changes to the compiler,
runtime, libraries and documentation. For writing Hirsel programs, see
[building and running programs](building.md).

## Layout

- `docs/spec/` is the specification of the core language, starting at `docs/spec/index.md`.
  Rule IDs such as `OWN-6` are defined once as a `### ID: Title` heading; `hirsel explain`
  prints them from the same text.
- `crates/` is the Cargo workspace. `hirsel-core-syntax`, `-resolve`, `-layout`, `-ir`
  and `-front` are the front end; `hirsel-core-driver` is the `hirsel` command;
  `hirsel-cranelift` and `hirsel-llvm` are the native engines; `hirsel-core-rt` is the
  runtime. Each crate's `README.md` lists its invariants.
- `core/core/` and `core/std/` are the core and std library sources the installation
  publishes. `core/corpus/`, `core/programs/` and `core/twins/` are the milestone
  workloads, their Hirsel programs and their Rust twins ([core/README.md](../core/README.md)).
- `bench/` holds the benchmark programs and their reference implementations.
- `tools/` holds the Buck2 build (`tools/buck2/`), the pinned LLVM toolchain
  (`tools/native-toolchain/`), the installation bundle (`tools/toolchain-dist/`) and
  the land and test tools.

## Toolchains

Rust is pinned in `rust-toolchain.toml` (1.99.0) and installed by rustup.
The LLVM engine needs LLVM 23.1.5 (`tools/native-toolchain/llvm-23.lock`);
`.cargo/config.toml` points `LLVM_SYS_231_PREFIX` at `tools/buck2/toolchains/llvm-files`
unless the environment already sets it. `tools/native-toolchain/README.md`
installs the pinned tree. AArch64 tests carry the pinned qemu and an AArch64
sysroot as Buck2 inputs; nothing is installed system-wide.

The build caps itself for a shared host: `.cargo/config.toml` limits Cargo
to 4 jobs and sets `RUST_TEST_THREADS` and `HIRSEL_JOBS` to 4. Export them
explicitly when a wrapper might override them:

```sh
export CARGO_BUILD_JOBS=4 HIRSEL_JOBS=4 RUST_TEST_THREADS=4
```

## Cargo

Cargo needs no build service. What it builds runs locally, so it runs under
[`tools/capped`](#run-locally):

```sh
cargo build --workspace
tools/capped --long --timeout 1800 cargo test -j 2 -p hirsel-core-syntax
```

## Growing inventories: no hand-maintained totals

Do not pin the size of a growing primitive, ledger, registry, provider, test or
API inventory in an assertion, array type or fixture footer. Check the identities
instead: compare inventories in both directions, reject duplicate rows, and
verify every declaration's contract or implementation. Keep fixed counts only
when they express a format, a bounded domain or a test's explicit input.

Use inferred arrays or slices for extensible lists. A checked-in inventory has
one item per line, sorted by identity, with no trailing total. Generate derived
inventories with the owning tool and verify them with `--check` or an exact drift
test; regenerate them after a merge instead of hand-merging their contents.
Sorted lists such as these merge without conflicts when contributors add or remove different
entries (the `hirsel` merge driver, `tools/buck2/merging.py`). None needs a total
updated when another contributor adds an item.

## Memory limits

Every hirsel binary and test harness caps its own memory before doing any work
(`crates/hirsel-guard`): a soft data limit of `HIRSEL_MEMORY_LIMIT` (bytes or a
`K`, `M`, `G` or `T` count; default `4G`), which programs they launch inherit;
only an AddressSanitizer executable is launched with it lifted
(`hirsel_guard::sanitized`), since its shadow mapping needs terabytes of address
space and the run's cgroup or pool reservation bounds it anyway. A
run that reaches it stops at once with `hirsel: memory limit 4G reached
(HIRSEL_MEMORY_LIMIT); ...` and exit status 71. Under Buck2 each test's limit is
its measured grant (twice its `memory_kb` in `tools/buck2/test_sizes.bzl`); a test
that needs more gets a re-measured request, not a variable.

## Run locally

The host is shared. A program run outside Buck2 (a built `hirsel`, a test binary, a
probe) runs through `tools/capped`, one program at a time:

```sh
tools/capped target/debug/hirsel run app
HIRSEL_MEMORY_LIMIT=2G tools/capped --timeout 60 target/debug/hirsel check app
```

It runs the command in a systemd user scope with `MemoryMax` of
`$HIRSEL_MEMORY_LIMIT` (bytes or a `K`, `M`, `G` or `T` count; default `4G`), no
swap, a `CPUQuota` of `$HIRSEL_JOBS` CPUs (default 2) and a timeout (`--timeout`,
default 300 s), and passes both variables on to the command. Without a reachable
systemd user manager it falls back to process limits (`RLIMIT_DATA` at the cap, a
thread limit and CPU affinity to `$HIRSEL_JOBS` CPUs) and says so. Concurrent runs
take numbered slots in the existing lock directory (`$XDG_RUNTIME_DIR`, or `/tmp`
when unavailable). The default slot count is `floor(MemAvailable / MemoryMax)`
at startup, clamped to 2 through 8; `HIRSEL_CAPPED_SLOTS` overrides it with any
positive count. A queued run prints each holder's pid, cwd, command and age,
the elapsed wait every five seconds, and its total wait on acquisition.
`--timeout` above 900 s requires `--long`, which takes a separate single slot
even with a shorter timeout, leaving the ordinary slots available to short jobs.
Slots are released on exit or crash. At exit it prints the scope's peak memory,
wall time and exit status. A run that reaches the cap or the timeout is a finding: report the program
and its peak rather than raising the cap. Runs over many programs go through their Buck2 targets, which run on the pool
within their measured reservations.

## Buck2

Hirsel builds and tests with Buck2. Cargo manifests and `Cargo.lock` stay
the source of truth: the BUCK files and the other build inputs
`tools/buck2/sync.py` derives are gitignored, and no change commits them
(`tools/buck2/README.md`, Generated files). `prepare.py` and `tools/land`
regenerate them when the tree changed; before a bare `.buck2/bin/buck2` after an
edit or a rebase, run `python3 tools/buck2/sync.py --if-stale`. The toolchains
(Rust, the pinned LLVM, Buck2 and its prelude) come in hermetically.

```sh
python3 tools/buck2/prepare.py                  # once per checkout
.buck2/bin/buck2 build //:hirsel-toolchain      # the installation prefix
.buck2/bin/buck2 test //:hirsel-core-syntax-unittest
tools/test-filter //:hirsel-core-front-unittest -- tests::m1_program_manifest --exact
```

Buck2 runs on a remote-execution pool whose endpoint, credentials and
platform properties live only in the gitignored `.buckconfig.local`, written
by a host hook (`tools/buck2/README.md`); without one `prepare.py` fails, and
`prepare.py --local` runs every action on this host instead. `-c hirsel.tests=host`
explicitly opts into running tests on the invoking host. More target names are in
[tools/buck2/README.md](../tools/buck2/README.md).

## Testing and landing

While iterating, run the one test that shows the change, by name, with
`tools/test-filter`. Before landing, `tools/affected` runs every target that
consumes the files the change touches, in one `buck2 test`. Run
`.buck2/bin/buck2 kill` in your worktree when you finish and before any wait
longer than a few minutes: idle daemons retain substantial memory on the shared
host.

A crate's own tests are `//:hirsel-<crate>-unittest` (and `-test-<name>` for its
integration tests). The native and milestone targets run real programs through
the driver on both engines and both targets: `//:core-driver-native-pool` and
`-openat2`, `//:core-m1-cranelift`, `//:core-m1-llvm`, `//:core-llvm-aarch64-m1`,
`//:core-m2b`, `//:core-native-laws-cranelift` and `-llvm`, and
`//:land-core-library-modes`, which installs a copied bundle and drives every
command in a clean environment. `tools/land` names the full acceptance set
(`CORE_ACCEPTANCE`); `tools/sweep` runs the longer suites on a schedule.

A change to LLVM emission must carry the matching Cranelift change.

### Known failures

Failures main is known to have are listed in `tools/known-failures.tsv` (one
tab-separated row each: test id, reason, owner, date). An id is a test
target or a binary and case
(`hirsel-core-driver-unittest::native::tests::scalar_core_instances_match_closed_bodies`).
`tools/consumers` classifies a red the list explains as known. If you
encounter a failure your change did not cause, add a row and push that row
straight to main. Remove the row when fixing the failure.

### Landing

[Landing a lane](landing.md) describes `tools/land` and `tools/land-run`: the FIFO
queue, the batch, the checks it runs and what it refuses.

No change commits a generated file, so no rebase conflicts on one: land
refuses a change that force-adds a gitignored build file, with
the commands that drop it. The files contributors commonly add to merge with
`tools/buck2/merging.py`, the `hirsel` merge driver `.gitattributes` names:
rows both sides append to `third-party/NOTICES.md` are both kept, and the sorted
ratchet and expectation lists keep the entries either side adds and drop those
either side removes ([hot files](hot-files.md)). Resolve any other conflict by hand.

`tools/main-stall-alarm` appends to `alarms.log` in `$HIRSEL_MAIN_RUNS` when main
gets no commit for 40 minutes while contributor worktrees are moving; it can run
from a repository maintenance loop.
