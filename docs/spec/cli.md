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

`hirsel run` uses a temporary executable in the compiler cache by default.
Pass `-o PATH` to keep the executable at a specific path.

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
another toolchain also stops the server. The server exits after a
request that leaves it above `--max-memory BYTES` (default 1 GiB)
resident, and after a request that panics. `hirsel serve --stop`
stops the store's server.

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
