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
copied kit and a bundle-relative recipe.

## CPU profiles

```text
hirsel prof run [package-dir] [build-opts] [--tasks] [--out FILE.hprof] [--freq 997] [--pmu LIST] [--stack-bytes 32768] [-- args...]
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
`--mem` selects sampled mode. `prof peak-mem` reads the memory results and
budget scopes from a profile. Both `prof run --mem` and `prof attach --mem`
ask you to record a tape and use `prof replay --mem`. Live workers can change
region tails and payer balances concurrently. Serializing a live run would
change its scheduling.

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
