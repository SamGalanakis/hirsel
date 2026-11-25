# Command-line interface

Run `hirsel help` for the command catalog. Use `hirsel help <command>` for
the options accepted by a command.

## Output paths

Build, pack, and SDK generation accept `-o` or `--output`. Use a file
path for `build` and `pack`, and a directory for `sdk ts`.

## Checking packages

`hirsel check` reports the number of definitions checked in the sources,
including cached checks. With `--json`, this is `checked_definitions`.
The `rechecked` field counts bodies recomputed on this request.

## Native build options

`hirsel run` uses a temporary executable in the request's store by default.
Pass `-o PATH` to keep the executable at a specific path. A build stores its
executable and detached debug file in the user's content store
(`$XDG_CACHE_HOME/hirsel/cas`) and delivers a reflink of it, or a copy where the
filesystem has none; a stored executable that fails its check is linked again,
never run.

Native binaries from both backends include `.eh_frame` unwind
information. Native builds write detached DWARF alongside the executable.
`--debug-lines on` also embeds DWARF. Failure positions are available in
either mode.
The native unwind records mark where a stack walk ends at a fiber boundary.

Register the host runtime with `core-product --install` or
`std-product --install` by passing `--link-kit DIRECTORY` alongside
`--runtime ARCHIVE`. The installed compiler links user programs with the
copied kit and a bundle-relative recipe. Installing the std product for a
target also compiles its release std to machine code, so release builds over
the installed std compile only the program. `std-product --store
CACHE --target TRIPLE` (or `--image IMAGE --target TRIPLE`) does the same for
that product's image and target.

`--logical-charge-report FILE` selects the installed charge observation
runtime for the build's target and recording mode. Product installations
register these archives with `--observed-runtime ARCHIVE` and
`--observed-recording-runtime ARCHIVE`. An explicit `--runtime` must name a
charge observation archive.

## Resident compiler

`hirsel serve [package-dir]` keeps one compiler process for a store
(`--store`, or `.hirsel-core` in the package directory). That process
answers the store's `build`, `run` and `test` requests and keeps in memory the
state those requests derive from unchanged inputs. The process listens
on `resident.sock` in the store and holds `resident.lock` while it runs.

`hirsel build`, `hirsel run` and `hirsel test` send their request to the store's
resident compiler when one is running, and otherwise compile in their
own process. `--resident` also starts a resident compiler when none
serves the store, and replaces one that refuses the request. A
resident compiler runs the request on the same path a one-shot
compiler takes, with the client's arguments, working directory,
environment and standard streams. Its output is byte-identical to a
one-shot build. A `run` request's program runs in the client process,
under the client's terminal and signals.

If the connection ends before the compiler answers, `--resident`
reconnects to a resident compiler, starting one if needed, and retries
compilation once. A `test` request or a request that can publish to a
durable code store or invoke a custom linker reports the lost answer. The
client starts a `run` program after receiving the complete launch answer.

A resident compiler refuses a request from another toolchain or another
environment. The client then compiles in its own process. A refusal for
another toolchain also stops the server. After each request, the server
evicts least recently used working sets until its resident memory fits
`--max-memory BYTES` (default 1.75 GiB). An exact memo entry shared by
several working sets stays cached while any of those sets remains.
If the limit is still exceeded after eviction, the server exits when
its launched clients finish. It also exits after a request that panics.
`hirsel serve --stop` stops the store's server.

## Dev loop

*`hirsel dev` (`--listen`, `--store`, `--fresh`, `--n-place`, `--json`) is built; `--sdk`<!-- doc-ref: planned -->,
`--wait`<!-- doc-ref: planned -->, `--status` and `--stop` remain planned. [launch](launch.md#the-dev-loop) is the full
text.* `hirsel dev`
rebuilds a local place as its sources change, keeps its store across sessions, and activates
each edit by the ordinary install and reopen. Its catalogue row reads "Rebuild a local place and restore its open subscriptions."

| Form | Does |
|---|---|
| `hirsel dev [SERVER] [--listen ADDR] [--store DIR] [--fresh] [--sdk DIR] [--n-place BYTES] [--json] [build-opts] [-- args...]`<!-- doc-ref: planned --> | runs the loop |
| `hirsel dev [SERVER] --wait [--timeout SECONDS] [--json]`<!-- doc-ref: planned --> | waits until the bytes on disk serve (0) or have failed (1) |
| `hirsel dev [SERVER] --status [--json]`<!-- doc-ref: planned --> | prints the loop's latest event |
| `hirsel dev [SERVER] --stop`<!-- doc-ref: planned --> | stops the loop |

`--n-place` (default 16 MiB) is the one allowance for describe, install, serve and SDK
generation. `--store DIR` is never deleted; `--fresh`<!-- doc-ref: planned --> starts a new
dev-owned store generation. `--sdk DIR`<!-- doc-ref: planned --> publishes the TypeScript SDK
for every activated generation by one directory exchange. Builds go through the resident
compiler. The build options `--libraries`, `--runtime`, `--source-core` and `--source-std` are
fixed for the session.

| Command | 0 | 1 | 2 | 75 |
|---|---|---|---|---|
| the loop | stopped by a signal or `--stop` | an I/O failure it cannot continue past | refused before serving | — |
| `--wait`<!-- doc-ref: planned --> | the edit serves | the edit failed | usage | no loop, or `--timeout` passed |
| `--status` | a loop answered | — | usage | no loop |
| `--stop` | the loop stopped | — | usage | no loop |

## Store commands

`hirsel store` commands operate on the directory of a log@1 store. They run in the
installation's `libexec/hirsel/store_tool`, which is granted only the two directories the driver
passes it. [STORE-SPEC §15](../../core/programs/store/STORE-SPEC.md) has the full contract.

| Form | Does |
|---|---|
| `hirsel store backup DIR TARGET` | copies the store at `DIR` into the empty directory `TARGET`, which it creates if absent, at the head it leases; prints `backup <store id> head <H>` |
| `hirsel store restore BACKUP DIR` | makes a store at the empty directory `DIR` from a backup; prints `restored <store id> head <H>` |
| `hirsel store inspect DIR` | prints the store's identity and head, then each table and index with its state, revision, exact row count and charged bytes; a read-only open, so it reads a store that is served |
| `hirsel store stats DIR` | the `inspect` lines, plus the journal's segments and bytes and the pack count |
| `hirsel store verify DIR` | scrubs a copy of the store and prints each fault |
| `hirsel store verify DIR --release EXE` | on a copy, declares the release's schema, scrubs, and checks every Public index against its table under the release's extractors; prints each changed revision, fault and wrong entry; changes nothing |
| `hirsel store repair DIR --release EXE` | on DIR itself: demotes each index with a wrong entry and builds every index to the end; refuses while a deploy's export lease is held |

A backup opens the store as its writer, so a store another process holds exits 75 and is not
changed. A store a place serves is backed up through its host, which is not built yet. A
restored store keeps its source's identity: restore replaces a lost store and does not clone one.
An interrupted restore leaves a directory that no `open` accepts; remove it and run the restore
again. Exit status: 0 done, 1 refused (the refusal is on stderr) or, for `verify DIR`, a scrub fault, 2 usage, 75 held. The `--release` forms exit as the release does when it refuses to start (nonzero, `store verify found N problems` on stderr).

## Coverage

*[Coverage](../coverage.md) is the full text.* A coverage build adds reached flags at
function entries and source control-flow arms. It is the same build
([CAP-9](capabilities.md#cap-9-recording-is-outside-the-program)), and no program reads a
flag (R28).

| Form | Does |
|---|---|
| `hirsel test [package-dir] --coverage [--json]` | builds the coverage build, runs the selected tests and reports reached entries and arms; with `--laws`, also each test's law rows |
| `--coverage-out FILE` | also writes the merged profile |
| `--coverage-base FILE` | adds a diff against an earlier profile |
| `--coverage-targets FILE` | restricts the uncovered list to the source ranges in FILE |
| `--coverage-runs` | keeps one row per run in the profile |
| `--seed-base K` | with `--seeds N`, runs seeds K to K+N-1 |
| `hirsel coverage merge FILE|DIR|@LIST... [--map FILE]... [--base FILE] [--targets FILE] [--json] [-o FILE]` | merges profiles, and raw files read against their site maps, and reports; runs nothing |
| `hirsel coverage diff BASE NEW [--json]` | lists the sites reached in one profile and not the other |
| `hirsel coverage diff PROFILE --test DEFINITION [--json]` | lists the sites only the profile's other tests reached, and only that test reached |

Each coverage option implies `--coverage`. A narrowed run is allowed. Each coverage test
build writes its site map beside its executable as `HEX.covmap`. Coverage never changes an
exit code: `hirsel test` exits as it does without it, and `hirsel coverage` exits 0 when it
reports, 1 when it refuses an input (another format version, or inputs from builds of
different sources) and 2 on usage errors.

`hirsel laws` stays static and about the lock: `check` and `write` run no test, and it has
no coverage option. Coverage never enters a law row or `hirsel.laws`.

## Adding a dependency

`hirsel add SOURCE [--root sha256:HEX] [--workspace] [--update] [--dry-run] [--package-store DIR] [--json]`
binds one dependency in the nearest package manifest and updates `hirsel.lock`. `SOURCE` is an
`https://` URL (one archive), a git source `git+https://HOST/PATH[?dir=DIR][#REF]` or its
shorthand `github:OWNER/REPO[/DIR][@REF]` (a commit, bound by `git` and `rev`; untagged, the
default branch's tip), a `.hpkg` path, a `sha256:` root already in the package store, a toolchain
`NAME@MAJOR`, or a listed member's directory. `--workspace` declares it in the root's
`[workspace.dependencies]` and binds the member with `{ workspace = true }`, one manifest at a
time. It verifies any archive and resolves the workspace before publishing the manifest. It
exits 0 for added, updated, unchanged or a dry run, 2 for a refusal before publication, and 1
for a failure after publication begins. [Packages](packages.md#adding-a-dependency)

## CPU profiles

```text
hirsel prof run [package-dir] [build-opts] [--tasks] [--mem[=sampled]] [--out FILE.hprof] [--freq 997] [--pmu LIST] [--stack-bytes 32768] [-- args...]
hirsel prof replay [package-dir] --tape TAPE [--tasks] [--mem[=exact|sampled]] [--out FILE.hprof]
hirsel prof attach <PID> [--duration SECS] [--freq 997] [--pmu LIST] [--debug PATH] [--out FILE.hprof]
hirsel prof summary <FILE.hprof> [--json]
hirsel prof tail <FILE.hprof> [--limit 20] [--json]
hirsel prof schema <FILE.hprof> [--json]
hirsel prof top <FILE.hprof> [--by self|inclusive] [--event NAME] [--group def_path|instance] [--filter GLOB] [--limit 20] [--json]
hirsel prof annotate <FILE.hprof> <DEF_PATH|FILE:LINE> [--json]
hirsel prof peak-mem <FILE.hprof> [--limit 20] [--json]
hirsel prof callers <FILE.hprof> <DEF_OR_SYM> [--depth 5] [--json]
hirsel prof callees <FILE.hprof> <DEF_OR_SYM> [--depth 5] [--json]
hirsel prof diff <BASE.hprof> <HEAD.hprof> [--by self|inclusive] [--group def_path|instance] [--min-delta-pct 1.0] [--json]
hirsel prof sql <FILE.hprof> "<SQL>" [--limit 500] [--timeout-ms 5000] [--json]
hirsel prof export <FILE.hprof> --format pprof --out <PATH>
```

For each target, `core-product --install` and `std-product --install` accept
`--prof-runtime ARCHIVE` and `--prof-recording-runtime ARCHIVE` to register
the profiling and combined profiling/recording archives. `hirsel env` lists
both archive paths. `hirsel --version` reports their content identities.

`prof run` takes the native build options of `run`. Its `--tasks` option selects the
profiling runtime and adds task, wake, host-stack and provider I/O rows.
When CPU sampling is unavailable, a task capture still runs and reports the
perf diagnostic in its quality header. It passes arguments after
`--` to the executable, leaves its standard streams connected, and returns
its exit status. `--out` names the profile and defaults to `profile.hprof`.
The command writes a [portable SQLite profile](profiling.md) after the
process exits. The sampler resolves verified detached DWARF, embedded DWARF
and ELF symbols.

`prof replay` executes the tape serially with the combined profiling and
recording runtime. `--tasks` captures runtime observations. The replay
verdict determines the command's status. A refused or divergent replay
leaves no profile.

`prof attach` snapshots mappings before sampling a running process without
stopping it. It ends the capture after `--duration`, at process exit, or on
Ctrl+C. `--debug` selects a detached DWARF file to verify when the executable
has no embedded DWARF.

For `prof replay`, use `--mem=exact` for exact peak counters and backing
lifetimes, or `--mem=sampled` for weighted allocation estimates. A bare
`--mem` selects sampled mode. `prof run --mem` samples every live worker and
reports weighted allocated and resident backing bytes at the peak and
last RSS reads, plus allocation rates by callsite. Live
memory is sampled only: `prof run --mem=exact` refuses and names replay.
`prof attach --mem` refuses, because attaching cannot sample allocations
made before it. `prof peak-mem` reads the memory results and budget scopes
from a profile.

Start with `prof summary` for data quality and the five hottest functions
per event. Use `prof schema` for tables and columns, and `prof sql` for
read-only queries. Query commands accept `--limit` and `--json`. The default
limit is 20 rows, or 500 for SQL. JSON includes a `quality` object and a
command-specific `result` object. Unsigned values above 2^53 use decimal strings.

`prof top` defaults to self cost and groups by definition path. `prof diff`
uses those same defaults, refuses incompatible targets, clocks and primary
events or units, and reports unmatched functions. A live-only capture
cannot be compared with replay. A single cycle or wall-time comparison
reports possible sampling noise.

`prof annotate` returns one row per source line, with saved source text and
separate `cpu`, `remarks` and `allocations` arrays. It sums CPU periods and
sample counts per event across the selected functions on each line.

`prof sql` accepts one read-only statement that returns rows. An SQLite
authorizer denies writes, ATTACH, and connection-changing statements.
The result includes the ordered column names, rows, row limit, returned row
count, and a truncation flag. The profiler interrupts a query that exceeds
`--timeout-ms`.

`prof export --format pprof` writes a standard uncompressed pprof protobuf to
the requested path.
