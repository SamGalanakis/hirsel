# Buck2 build

The workspace builds and tests with Buck2, on this host or on an optional
remote-execution pool (for example NativeLink, with a shared content-addressed
cache). Cargo manifests and `Cargo.lock` stay the source of truth: `sync.py`
derives every Buck2 build file from them, and `cargo build` keeps working.

## Generated files

No lane commits a generated file, so no rebase can conflict on one
(`generated.py`):

- **Build inputs are gitignored and generated on demand.** `sync.py` writes the
  root and `third-party/` `BUCK`, `third-party/Cargo.{toml,lock}` and `lib.rs`,
  `exec_sizes.bzl`, `test_sizes.bzl`, `workspace-metadata.json` and
  `tools/native-toolchain/pins.bzl`. `bootstrap.py` runs `sync.py --if-stale`.
  Generation has two parts, third-party dependencies
  (Cargo metadata, then Reindeer) and the first-party inventory (Cargo
  metadata, then source inspection), and a stamp in `.buck2/` records each
  part's outputs and every file, existence test and glob it read. `--if-stale`
  regenerates only a part whose outputs are missing or edited or whose inputs
  read differently, so a documentation edit regenerates nothing and a Rust
  source edit reruns only the inventory; either part replaces an output only
  when its bytes change. `sync.py` without flags regenerates both parts. After
  you edit sources or rebase, `.buck2/bin/buck2 build|test` checks the stamp
  before starting Buck and refuses stale inputs with the command
  `python3 tools/buck2/sync.py --if-stale`. It compares content digests,
  existence probes and glob matches; fresh inputs are never regenerated.
  `kill`, `status` and `log` bypass the check. Use `prepare.py` in a fresh
  checkout to install the wrapper and generate the build files.
- **Files many lanes add to merge themselves.** `.gitattributes` gives
  `third-party/NOTICES.md` and the `modules.rs` registries the `hirsel` merge
  driver, which `bootstrap.py` installs. The driver keeps notice additions from
  both sides. For module declarations it also preserves removals.

## Setup per checkout or worktree

```sh
python3 tools/buck2/prepare.py        # bootstrap.py, then the remote-execution hook
python3 tools/buck2/prepare.py --local  # or: no pool, every action on this host
```



A new worktree needs `prepare.py`: unlike `sync.py`, it installs the pinned
client, prelude, Rust, LLVM (including the hermetic C++ headers) and Reindeer,
and configures execution. The wrapper also refuses a missing or stale LLVM
receipt or required file with that command; it does not bootstrap automatically.

A pool's endpoint and credentials never enter the repository. `prepare.py`
runs an optional hook, the executable named by `$HIRSEL_BUCK2_REMOTE_CONFIG`
or `~/.config/hirsel/buck2-remote-config`, with the checkout's root. The hook
writes the gitignored `.buckconfig.local` (only when its content changes):
the `[buck2_re_client]` endpoint, instance and TLS identity, and
`hirsel.remote_properties`, the `key=value,...` platform properties the
pool's workers match. When the hook is missing or fails, an existing remote
`.buckconfig.local` is kept; without one `prepare.py` fails and names the
missing hook. Only `prepare.py --local` writes `hirsel.execution = local`,
which runs every action on this host.

Toolchain preparation runs on the pool like any compile: the third-party crate
`http_archive`s, the `native_toolchain_unpack`s and the `link_kit`
(`native-toolchain/defs.bzl`) resolve to the default pool platform, so every
checkout after the first takes their outputs from the remote action cache and
never downloads or unpacks them here. Only targets that name
`//tools/buck2:host` (tests that run the host's own tools) stay local, uncached:
the pool's client listener serves the action cache read-only, so a local
result could not be uploaded (`allow_cache_uploads`) anyway.

## Formatting before a commit

`python3 tools/buck2/fmt.py` formats Rust with the pinned rustfmt; `--check`
makes it read-only. It also checks changed `.hirsel` sources under the core
library, front-end case and program roots (`SOURCE_ROOTS`) with the candidate's
`hirsel-core-syntax` formatter, and never rewrites them. Refusal and gap fixtures
(an `// expect: refuse|gap|resource` comment) are input bytes and excluded. The
selection includes branch commits since the merge-base with `origin/main`, staged
and unstaged changes and untracked files. A Rust-only change needs no
formatter build.
The same command checks the core sources and spec fences with the editor
parser via `editors/tree-sitter-hirsel/check-local`.

## Stale outputs

Buck2's scheduled stale sweep runs hourly, first 6 minutes after daemon start
(`clean_stale_enabled = true`). Each tick scans `buck-out` and deletes inactive
tracked outputs older than the TTL and every path the materializer does not
track. A running local action's outputs are untracked, so the pinned release
skips a tick while any command holds the daemon (`BackgroundCleanupGate` in
`deferred.rs`): build, test and the other streaming commands except `lsp` and
`dap` register a prevention guard that interrupts a running clean and waits for
it, and `Op::CleanStaleRequest` in `deferred/command_processor.rs` starts a clean only
when no guard is held. Releases before 2026-11-01 had no such gate. On
2026-11-25, under the 2026-09-15 pin, 33 failures in 15 worktrees each followed
a tick (rustc's `could not write output to .../extras/...` and `couldn't create
a temp dir`, clang's `unable to rename temporary`), and a strace of a repro
daemon showed its own threads unlinking an in-flight `output_artifacts`
directory; the sweep was off from 99e5e724 until this pin.

Reclaim space between ticks, with no build running in the checkout:

```sh
.buck2/bin/buck2 clean --stale --tracked-only
```

It applies the configured policy and walks only tracked artifacts: outputs not
used for six hours; at 10% free disk or less, adaptive cleaning promotes the
oldest inactive outputs until projected free space recovers, keeping anything
accessed within one hour. Active artifacts stay protected, including under disk
pressure. Untracked leftovers (outputs of actions a killed daemon never
declared) are not reclaimed this way; deleting the worktree's `buck-out` is.
Lanes never delete another lane's `buck-out`. The policy is read at daemon
startup; an existing daemon needs a restart.

The settings were checked against the exact `pins.json` source commit
[`5b3b13d3eacefe325bf971369197e33c5f4b3968`](https://github.com/facebook/buck2/tree/5b3b13d3eacefe325bf971369197e33c5f4b3968),
and the installed executable's SHA-256 matched the pin. Source references below
are relative to that commit:

- `app/buck2_execute_impl/src/materializers/deferred/clean_stale.rs`,
  `CleanStaleConfig::from_buck_config`: `clean_stale_enabled` and both adaptive
  protection switches are booleans; `clean_stale_artifact_ttl_hours`,
  `clean_stale_period_hours`, `clean_stale_start_offset_hours`, and
  `clean_stale_low_disk_adaptive_min_ttl_hours` are nonnegative floating-point
  hours. `clean_stale_low_disk_adaptive_enabled` is a boolean.
  `clean_stale_low_disk_threshold` is a floating-point **free-space percentage**,
  so 10 engages at 90% full. `clean_stale_low_disk_artifact_ttl_hours` is an
  optional floating-point hour value for fixed mode, ignored in adaptive mode;
  we leave it unset. The two protection switches are
  `clean_stale_low_disk_adaptive_delete_intermediate_within_min_ttl = false`
  and `clean_stale_low_disk_adaptive_unmaterialize_active = false`.
- That file's `create_pending_clean_result` requires SQLite state and deferred
  writes. `app/buck2_server/src/daemon/state.rs` creates the deferred
  materializer, but **`defer_write_actions` defaults to false**. We explicitly
  set it and `sqlite_materializer_state` to `true`; both accept booleans via
  `RolloutPercentage` (`app/buck2_core/src/rollout_percentage.rs`). SQLite state
  defaults to true (`app/buck2_server/src/daemon/disk_state.rs`). Changing the
  deferred-write setting invalidates old persisted materializer state, so the
  first scan also removes untracked outputs after current outputs are declared.
- `StaleFinder::visit` and `find_stale_tracked_only` require `active: false`
  before marking tracked artifacts stale. `apply_adaptive_low_disk` promotes
  only `TrackedState::Retained`; it returns before considering active artifacts
  when `unmaterialize_active` is false. Declarations and materialization mark
  current artifacts active (`deferred/command_processor.rs`); state loaded
  after a daemon restart begins inactive (`deferred/artifact_tree.rs`).
  The scheduled sweep (`Op::CleanStaleRequest` in
  `deferred/command_processor.rs`) runs with `tracked_only = false`, so it
  deletes untracked paths, but only after `BackgroundCleanupGate::try_start_cleanup`
  finds no command holding a prevention guard (`deferred.rs`; taken in
  `daemon/server.rs` for streaming commands). An explicit
  `buck2 clean --stale` is not gated against a build in another client.
- `--materializations none` controls which final outputs Buck places on disk.
  The host remote-config hook writes only `[buck2_re_client]` and `[hirsel]`.

Six hours preserves recent rebase outputs without retaining days of hashes.
One hour under disk pressure retains recent reuse while reclaiming older
inactive outputs. The policy cannot reclaim an active set kept alive in one
daemon indefinitely.

Validation on 2026-10-20 used a disposable copy of `fs-watch-mmap/buck-out`,
with a consistent read-only SQLite backup, in `buck-clean-stale`. After building
`//:hirsel-toolchain` and `//:hirsel-span` and testing `//:hirsel-span-unittest`,
allocated `buck-out` size (`du -s -B1`) fell from **22,952,734,720 bytes
(21.376 GiB)** to **3,068,723,200 bytes (2.858 GiB)**, an **86.63%** reduction:

```sh
.buck2/bin/buck2 clean -c buck2.clean_stale_enabled=false --stale 6h \
  --adaptive-low-disk-threshold 10 --adaptive-min-ttl 1h
```

Only the periodic schedule was disabled during measurement to isolate the
explicit scan. Buck reported 5,292 untracked paths removed (19.6 GiB logical
size); switching deferred writes had invalidated the copied old database.
Thus this measures the initial migration cleanup, not steady-state tracked
TTL eviction. The policy also passed a configured-policy dry run. A zero-TTL
adaptive dry run found zero stale paths and retained all 544 active/recent
artifacts. All three current CLI, std image and library output checksums stayed
identical. The incremental build and test then used zero build actions; the
unit test passed before and after (one test each). After restarting the daemon
with the committed policy, the same build hit 604 cached actions, with zero
remote executions (172 local preparation actions); the test passed again.
The copied runtime archives accounted for about 8.16 GiB and toolchain bundles
3.36 GiB before cleanup; sharing these across worktrees is separate work.

## Distribution compiler

Native runtime archives use a separate `profile_runtime` on both target
architectures: fat LTO, `opt-level=3` for the runtime and its live-operation
dependencies, and `opt-level=s` for run-config, run-schema and the Serde codecs.
The allowlist and flags live in `profile.bzl`; generated programs keep the
native compiler's speed optimization. Compiler dev/release/dist profiles and
host build scripts are independent of this archive profile.

`tools/toolchain-dist/build --pgo --output /tmp/hirsel-dist` builds the shipped
compiler with `//tools/buck2:dist`: fat LTO, one codegen unit and PGO trained on
compiler-bench's examples and syn3k. The existing mimalloc dependency already
builds v3. The distribution builder packages this profile.
Developer `dev` and `release`, checked tools and runtime archives keep their
existing profiles. See [the pipeline](../toolchain-dist/README.md) for exact
commands, profile provenance, interleaved measurements and the declined BOLT trial.

## Everyday commands

```sh
export CARGO_BUILD_JOBS=4 HIRSEL_JOBS=4 RUST_TEST_THREADS=4
.buck2/bin/buck2 build //:hirsel-toolchain --show-output   # the installation prefix
.buck2/bin/buck2 test //:hirsel-core-syntax-unittest       # one crate's unit tests
.buck2/bin/buck2 test -c hirsel.tests=focus //:hirsel-core-front-unittest -- --test-arg=tests::m1_program_manifest --test-arg=--exact
.buck2/bin/buck2 test //:core-driver-native-openat2   # bodies that need the host's openat2, on this host
python3 tools/buck2/sync.py --if-stale            # the generated build files, after any edit or rebase
tools/affected                                    # every target the change can affect
python3 tools/buck2/fmt.py                       # format before pushing
```

Builds use Buck2's default isolation dir, `v2`: never pass
`--isolation-dir` (or pass only `--isolation-dir v2`) and leave
`BUCK_ISOLATION_DIR` unset. Each worktree already has its own `v2` daemon.
The isolation dir is part of every action's output paths, so an action run
under another one has a different key and misses the pool's shared cache. A
private isolation dir is only for inspecting the graph beside a running
command, or for escaping a stuck daemon; anything it builds starts cold.

`-c hirsel.tests=focus` is for running one test or a few by name: each test
run books one CPU, with its binary's measured memory, instead of the binary's
whole-binary request. The pool
reserves nothing for a multi-CPU action, so a four-CPU run of one case waited
for one worker to free four CPUs at once: over twelve alternated edits its
queue had a median of 43 s and a maximum of 353 s, against 0.4 s and 6 s in
focus. Leave it off for whole crates and gate stages, which use the threads
their request buys; the request is in the test's action key, so a focus run
neither reuses nor replaces their cached verdicts.

Local commands run through `tools/capped` with memory and CPU-share limits,
without CPU-second or wall-time limits. The progress reporter has no inner
deadline. The pinned Buck2 client still requires a test-execution timeout;
all tests share the single 86400-second outer bound in `.buckconfig`. The
repository executor passes it to the stock runner because this pin ignores
`test.timeout_default_s` for external test providers. `prepare.py` records the
executor's absolute checkout path; no Buck patch or per-target override is used.

Arguments after `--` go to Buck2's test runner: `--test-arg=<a>` passes one
libtest argument. Use `tools/test-filter BINARY -- FILTER [LIBTEST FLAGS...]`
to select compiled cases and compute their owners. It preserves substring,
exact, skip and ignored selection and requires every selected body once.
`--list` prints the selection without running it. Tests run on the pool with
their measured requests, except the explicitly declared openat2 bodies in
`test_requirements.bzl`, which run on the invoking host. `-c hirsel.tests=host`
opts into host execution for other tests, with the same pinned tool inputs.
Target names come from Cargo; `-unittest` and `-test-it` run unit and integration
tests, and the library target runs declared doctests.

`tools/consumers <path>...` prints affected test targets using Buck reverse
dependencies and direct source ownership. Run them with
`tools/consumers <path>... | xargs .buck2/bin/buck2 test`. Output keeps the 100
targets nearest the changed owners, with direct owners first. `--limit N`
changes the cap and `--limit 0` keeps all. A capped list reports what it cut
and proves only partial consumer coverage. Discovery runs no bodies. A stale
libtest selector fails before dispatch. A Rust target that completes no test
bodies fails at result classification, including empty binaries and ignored-only
runs. A host-only partition with no selected bodies keeps its empty status.

`test_report.py` streams libtest JSON start/result events without an inner
deadline. Ordinary result lines, summaries and per-case timings remain available
to count and cost tools. Python unittest scripts retain their verbose output.
On an external signal, the reporter prints `HIRSEL_TEST_INTERRUPTED`, completed
results, active body names and phase spans, then reaps only its owned descendants.
Descendants keep the executor's process group for memory enforcement. The
pinned client's outer test timeout remains as described above.

Native tests receive LLVM 23.1.5, the Arm GNU 14.3.Rel1 Linux sysroot and static
AArch64 qemu as SHA-pinned Buck2 downloads or builds from them. No worker image
changes, PATH lookup or binfmt registration are needed. The launcher resolves
`LLVM_SYS_231_PREFIX` and each variable of `native-toolchain/environment.tsv`
(a path inside one toolchain) to absolute input paths before changing directory.

The opt-in `//:hirsel-core-rt-aarch64-harness` builds a real ARM libtest
executable. The ordinary runtime unittest harness intentionally remains x86.
Build it with `buck2 build --show-json-output` and run the returned executable
through the pinned `HIRSEL_QEMU_AARCH64 -L HIRSEL_AARCH64_SYSROOT` runner.
Export both variables for child fixtures, which reexecute through that same
runner without binfmt registration. The ARM C dependencies and linker use the
pinned cross GCC sysroot; host tool dependencies remain x86. Real-file bodies
execute their openat2 precondition and fail if it is unavailable.

The LLVM pin check is `//:gate-native-toolchain-pin`;
the standalone tool proof is `//tools/buck2/native-toolchain:proof` (see
[the native toolchain](../native-toolchain/README.md)).

Target names follow Cargo's targets, all in the root package:

| Cargo target | Buck2 target |
|---|---|
| library of `hirsel-x` | `//:hirsel-x` |
| its unit tests | `//:hirsel-x-unittest` |
| `tests/it` (or `tests/<name>.rs`) | `//:hirsel-x-test-it` (`//:hirsel-x-test-<name>`) |
| binary `<bin>` and its unit tests | `//:hirsel-x-bin-<bin>`, `//:hirsel-x-bin-<bin>-unittest` |
| example `<ex>` | `//:hirsel-x-example-<ex>` |
| build script | `//:hirsel-x-build-script` |
| a host-routed openat2 suite | `//:<name>-openat2`, with `//:<name>-pool` for the rest |
| a script check | `//:gate-<name>` or `//:test-<name>` |

## How the graph is made

- `sync.py` writes `third-party/Cargo.toml` (every external crate a member
  names, at its locked version with the features Cargo resolved for the
  workspace), `third-party/Cargo.lock` (from the workspace lock, so no version
  moves), `third-party/BUCK` (Reindeer, configured by
  `third-party/reindeer.toml` and `third-party/fixups/`) and the root `BUCK`,
  all gitignored (Generated files above).
- Native and full-gate crate cases use the same pool execution path as ordinary
  tests. The runtime ABI symbol test reads the separately built production
  archive; the layering test reads `workspace-metadata.json`, generated from
  Cargo's workspace graph by `sync.py`. Neither
  invokes Cargo on a worker.
- A pool test's passing verdict is cached by its action key.
- `cargo_units.py` asks the pinned Cargo for unit-graph v1, without compiling,
  for each package's normal build and its package-local tests. Production
  receives only normal dependency features; tests receive their own declared
  dev features. Build-script and proc-macro host contexts stay separate when
  Cargo resolves them differently. Runtime dependencies resolve for both
  architectures. Units share a Buck identity only when their features and
  dependency identities match; additional identities use `-features-<hash>`.
  Resource requests continue to use the original crate's measured budget.
  Reindeer supplies source, build-script and native-fixup templates, whose
  Rust feature sets and dependency edges come from those Cargo units.
  `third-party/units.json` keeps external labels available when only the
  first-party source inventory needs regeneration. The production feature
  audit admits no exceptions.
- All first-party targets are in the root package, so a compile action's source
  tree keeps repository paths and `include_str!("../../../core/x")` works
  unchanged. A target's sources are its directory (`src/`, `tests/it/`,
  ...) plus every file its literal `include*!` and `#[path]` references reach
  (and the files an `include_str!(concat!("<prefix>", $arg))` macro names
  through its file's string literals).
- Tests run as Cargo runs them, from their package directory. Every path
  Cargo compiles in absolute is relative to that directory instead (rustc's
  `--env-set`): `env!("CARGO_MANIFEST_DIR")` is `../../crates/<crate>`, which
  names the crate from any package directory, and so are `CARGO_TARGET_TMPDIR`
  and `CARGO_BIN_EXE_<name>`. Tests read only the files they declare as data
  (`RUNTIME_DATA` in `sync.py`).
- `CARGO_BIN_EXE_<name>` names `buck-out/tmp/cargo-bin-exe/<harness>/<name>`,
  and the launcher links that to the binary the test run carries. The
  harness compile does not depend on the package's binaries, so it neither
  waits for their links nor fetches them. A harness run outside the launcher
  does not find them.
- Executables link with zlib-compressed debug sections
  (`--compress-debug-sections=zlib`): half the bytes per whole-compiler
  binary, and std's backtraces still print file and line. zstd sections lose
  the locations.
- Test harnesses are `rustc --test` binaries; `test_launcher.sh` runs them
  with an existing `CARGO_TARGET_TMPDIR`, the pinned LLVM as
  `LLVM_SYS_231_PREFIX` (its `bin/` first on `PATH`, its `lib/` on the loader
  path), insta's workspace root, and test threads and `HIRSEL_JOBS` set to the
  action's effective CPU reservation (`HIRSEL_TEST_CPUS`, passed from the same
  request as the executor, including focus mode), at most 4. A visible cgroup
  quota or a smaller caller setting can reduce it; command-line
  `--test-threads` cannot exceed it. Pool actions require the explicit
  reservation, since their sandbox need not expose `cpu.max`.
  It asks the compiled harness to apply caller filters.
- On the pool an action's work directory is a bind mount of a subdirectory.
  The runtime refuses to record there, since it cannot establish the mount's
  source ancestry, so the launcher puts
  `CARGO_TARGET_TMPDIR` and `TMPDIR` on the action's private `/dev/shm`.
- First-party binaries carry an `$ORIGIN`-relative RPATH to
  `tools/buck2/toolchains/llvm-files/lib`, so a test's child processes find
  libLLVM with a cleared environment; tests on the pool carry that directory
  (`toolchains//:llvm-libraries`).
- The driver's build script runs over the files it hashes, laid out at their
  repository paths (`hirsel_build_script` in `defs.bzl`); its output directory
  is the driver's `OUT_DIR`.
- llvm-sys's build script is replaced by its fixup: `wrappers/target.c`
  compiles against the pinned headers and binaries link the pinned libLLVM
  (`toolchains//:libllvm`), as `force-dynamic` selects under Cargo.

Workspace build scripts publish only their generated files in `OUT_DIR`.
Their unused Cargo rerun directives are discarded: those directives contain
absolute action-sandbox paths and filesystem traversal order, so keeping them
would change consumer input digests under an identical build-script action key.

`//:hirsel-core-rt-archives` builds the core runtime's static PIC archive for each
target from Buck2's `RustLinkInfo` and strips its debug sections
(`runtime_parts.py`), so compiled Hirsel programs link one self-contained
`libhirsel_core_rt.a`. Its views `[x86_64]` and `[aarch64]` hold one archive each;
the aggregate holds both for the workspace build and the toolchain bundle.

## Toolchains and execution

- Rust: the official 1.99.0 components (rustc, std for x86-64 and AArch64,
  clippy, rustfmt), byte-identical to the rustup ones, from
  `hermeticbuild/rust-redist`.
- C and linking: clang 23.1.5 from the pinned LLVM tree
  (`tools/native-toolchain/llvm-23.lock`), built in manylinux_2_28. The tree
  contains stock libLLVM for the build tools and the optimized engine for Hirsel.
- `platforms.bzl`: one remote execution platform per measured `(CPU, memory,
  CPU class)` budget, with a 1 CPU/1 GiB compile default, 1 CPU/512 MiB Clippy
  twins and measured test requests (below), carrying the pool's platform
  properties. Only the critical-path compiles named in `CPU_CLASSES`
  (`action_sizes_from_log.py`) and the std precheck (`PRECHECK_REQUEST`, 1 CPU
  and 2 GiB) ask for `cpu_class=fast`; other tests
  and gate shards run on any worker. Hirsel lanes queue remote compiles and
  tests at priority 50, below the pool's ordinary lane class (100), above
  its warming class (0), which gets no reserved worker. The scheduler merges
  identical actions at the first submitter's priority. `-c hirsel.priority=N`
  accepts 0 through 99. A test run carries its
  own executor (`test_executor` in `platforms.bzl`): Buck2 runs tests with it,
  not with their platform's. The priority is not part of an action key. The
  platforms also include the invoking host for an explicit
  `-c hirsel.tests=host` opt-in. Native tools are pinned action inputs in
  either mode; no test needs those tools installed on the worker image.
- Profiles (`profile.bzl`): the default target platform is
  `//tools/buck2:checked`, `[profile.checked]`: release O3 with debug
  assertions and overflow checks. Every first-party library and binary, test
  harnesses and the executables they launch (`CARGO_BIN_EXE` subprocesses)
  build once in it, so
  builds and tests share compilation actions. Integration
  test crates (`tests/`) compile their own code at opt-level 0 over O3
  libraries (`_test_crate_flags` in defs.bzl); unit test crates are the
  library and keep O3. `//tools/buck2:release` is `[profile.release]`. There
  is no Buck dev configuration: Cargo's `[profile.dev]` serves Cargo builds
  only. Runtime archives keep the production runtime profile. Proc macros and
  build scripts keep the host configuration (`host_transition`,
  `profile_dev`). The std image pins its exec dependency to this same checked
  configuration: an ordinary `exec_dep` inherits the execution platform's
  configuration, which has no Rust profile. Checked compiles use the dev
  compile requests and codegen policy, preserving their memory headroom and
  parallelism while enabling O3.
  Canonicalizing test harnesses on entry also keeps the workspace build and
  tests from compiling the same harness under different reservations.

## Prelude overlay

The prelude cell is the one bundled in the pinned executable, expanded to
`.buck2/prelude` (`.buckconfig`: `[external_cells] prelude = disabled`) and
patched by `prelude_overlay.py`. Command wrappers use `/bin/bash` from the
pool image, so rustdoc can invoke its linker without a `PATH`. The LLVM tool
wrappers also locate their tree using shell builtins. It moves helper work off
the pool:

- `rust_library.bzl`: successful doctest verdicts are cached like libtests.
  Their executor requests one CPU and 512 MiB at the same priority as other
  tests, and a stable `TEST_TARGET` lets pool usage name them.
- `failure_filter` (`rust/failure_filter.bzl`): the daemon reads a compile's
  build status; a passing compile's output is a declared copy, and only a
  failed one runs the stock action, which replays rustc's diagnostics.
- `deps` (`rust/build.bzl`): each compile's `-Ldependency` tree is laid out by
  the daemon with `ctx.actions.assembled_dir`, the same tree the stock remote
  action wrote, so consumer action digests do not change. It refuses
  `crate_dynamic`, which no target uses.
- Compiler and rustdoc actions no longer carry extracted `third-party` crate
  sources (`rust/sources.bzl`, `rust/build.bzl`): rustc reads dependency text
  only at repository paths, where those archives never are.

`pins.json` (`prelude_overlay`) pins each patched file's stock and overlaid
SHA-256, and every transform replaces a stock text found exactly once, so a
prelude pin change fails `prepare.py` instead of mis-patching.
`tools/tests/prelude_overlay.py` covers the guards. After a Buck2 upgrade,
expand the new prelude, port the transforms, and re-pin with
`prelude_overlay.py --print-pins --prelude-dir <stock prelude>`. To drop the
overlay, set `[cells] prelude = prelude` and `[external_cells] prelude =
bundled`, and delete `prelude_overlay.py`, its pins, its test `tools/tests/prelude_overlay.py`, and
`prepare_prelude` in `bootstrap.py`.

## Measured test requests

A pool test binary reserves what it measured using: `test_sizes.bzl`
(`TEST_REQUESTS`, which `sync.py` derives from the evidence in
`test-sizes.json`) gives each binary, shared by
its shards, one CPU while total CPU seconds / total wall seconds is at most
1.6, otherwise two. A one-CPU action may burst to two (cpu.max = 2 × request).
A short CPU burst does not establish sustained parallelism. Four-CPU full-run
shards no longer have an exemption; the retained matrix uses four nonempty slices per engine
of 96 long-lived four-CPU reservations. The launcher runs as many test threads
as the reservation's whole CPUs.

Memory reserves the largest measured process-group PSS peak times 1.2, rounded
up to 64 MiB, at least 128 MiB. The worker's twice-reservation kill threshold
is a safety mechanism, not a discount on the reservation. The 20% margin
covers PSS sampling jitter and moderate code growth; fine rounding avoids a
256 MiB rounding penalty on small binaries. Unmeasured native stages inherit
the largest freshly measured native sibling, explicitly recorded as a fallback.
Other binaries without a row retain the 1 CPU / 2 GiB conservative default.

The process data/address-space guard (HIRSEL_MEMORY_LIMIT) normally uses
that kill line, with a 1 GiB guard floor retained independently of PSS.
guard-stops.json records allocation limits including unused
thread stacks, which can far exceed resident PSS. Those now raise only the
guard limit (TEST_GUARD_LIMITS), independently from the reservation. Completed
memory-kill retries are sized from measured peaks, without preserving the
scheduler's geometric escalation overshoot forever. Unfinished memory kills
keep a conservative floor of twice their failed request times 1.2.

Pass read-only copies of the remote-execution logs. The worker usage logs
(`actions.log` and `actions.log.1`) report the test target and CPU per run. The
scheduler's JSONL trace reports peak PSS by operation. The optional worker
executor journal supplies memory-kill records.

```sh
python3 tools/buck2/test_sizes_from_usage.py --usage /tmp/usage-*.log \
  --pss /tmp/trace-*.jsonl --kills /tmp/kills.txt
# Refresh selected binaries without discarding other measurements:
python3 tools/buck2/test_sizes_from_usage.py --only hirsel-core-driver-unittest \
  --usage /tmp/usage-*.log --pss /tmp/trace-*.jsonl
```

A request is part of the test's action key: resizing reruns those tests once.
The resized binaries' consumers select them directly; evidence-only changes run
tool checks.

`pool_usage.py` reports, read-only from the same trace and usage rows, the
pool's reserved CPU-hours by repository and hirsel's reserved against used
CPU-hours and GiB-hours per lane and target over the last N hours.

It also sums execution wall seconds and CPU seconds per action category,
excluding queue time. For rustc compile/link phases and target names, it joins
the shared retained Buck2 event logs. The recorder stores compressed
`.pb.zst` logs, including failed runs, under `$HIRSEL_BUCK2_LOG_DIR`
(default `~/.cache/hirsel-buck2/event-logs`, shared across worktrees). Rotation keeps at
most three days and 4 GiB, oldest first; running writers and audit readers keep
their files locked until finished, so active logs may temporarily exceed the
size bound. A new run rotates completed logs after it finishes.

Select another archive with `--event-dir`, or add individual compressed or
JSONL logs with `--events`:

```sh
python3 tools/buck2/pool_usage.py --hours 6 --trace /tmp/trace-*.jsonl \
  --usage /tmp/usage-*.log --events /tmp/build.pb.zst
```

To read logs over SSH, pair `--ssh-cache` with `--remote-trace-dir` and
`--ssh-workers` with one or more paths under `--remote-usage`.

This reuses the compile-sizing tool's unique time/request match, separately for
each action category, so native observations and other helpers also get their
target names. Unmatched or ambiguous rustc measurements stay in `rustc (phase unknown)`. It also reports
repeated target/action-digest executions from retained compile and test events;
cache hits and copies of the same merged execution do not count as reruns.
The report states its log coverage; rotated history cannot be reconstructed.
Rustc's link
action includes compiling the executable, so its time is not linker-only.

## Measured compile requests

`action-sizes.json` keeps dev/release budgets per target, with phase timings,
CPU seconds and PSS memory peaks. `sync.py` generates `exec_sizes.bzl`
from it; compile rules select that target's platform. A dev compile splits its
code into 16 codegen units whatever its reservation, so LLVM spreads them over
the reserved CPUs, and one reserving more than one CPU runs rustc's front end on
that many threads (`-Zthreads`; `_dev_codegen` in `defs.bzl` has the
measurements). A release compile's codegen units match its reservation;
unmeasured release targets keep Cargo's defaults.
One target's metadata and codegen share the reservation.
Cargo profiles and dependency resolution remain in Cargo.toml.

The compile generator joins exported Buck2 commands to dispatches by action
digest and start time, then to worker CPU and scheduler PSS measurements by
operation. Only uniquely joined successful executions inside the explicit
measurement window count. Both first-party and third-party compiles get
per-target dev/release requests. A profile with no fresh executions inherits
the measured sibling profile's memory before using the generic default;
its CPU request remains one until measured in that profile. Missing or
ambiguous matches stay visible. Retries of one operation are one join candidate;
their largest PSS peak includes killed attempts. Refreshing an existing evidence
file retains unobserved targets and special classes, including on-demand native
observations.
Decode retained event logs with event_logs.commands and copy existing worker
logs and scheduler traces; do not launch a workspace build to collect data.
Run action_sizes_from_log.py with --commands, --pss, --usage, --since and
--until, writing action-sizes.json, then regenerate with sync.py.

CPU follows sustained use across the complete run: one CPU through 1.6 cores,
otherwise two. Old October 16 calibrations no longer pin four CPUs; a new
four-CPU request needs fresh evidence that it shortens the critical path.
Memory is maximum measured PSS times 1.2, rounded to 64 MiB, at least 128 MiB.
The unmeasured compile default is 1 GiB: new harnesses and generated targets
exceeded the former 256 MiB reservation repeatedly. There is no fixed
512 MiB anonymous-memory surcharge. special_requests in the same evidence
sizes precheck, Clippy and native observation from their own measured peaks.

Priority and lane attribution do not enter the action digest. An action keeps
the first submitter's priority when a later invocation requests it. Dependency
symlink trees are assembled in the daemon by the pinned prelude overlay,
without remote compile-sized helper actions. Clippy twins share ordinary
dependencies.

`clippy.py` builds each target's Clippy twin `<name>-clippy`: the same rule on
its measured single-CPU platform, declared only under `-c hirsel.clippy=true`, so
Clippy never reserves a compile's request and `buck2 build //:` never
compiles the twins.
