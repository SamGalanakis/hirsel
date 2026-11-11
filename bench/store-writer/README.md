# Store writer baseline

Run from the repository root, with `perf`, `cc`, the pinned Buck toolchain, and passwordless `sudo perf probe` available:

```
bench/store-writer/run --output /workspace/code/hirsel-briefs/store-bench-678/run-$(date +%s) --profile
```

The output directory must be on the local disk to measure. Each database is new.
The runner builds the release compiler and LLVM native executable, then runs one
sample per workload in each engine and Rust. It restricts affinity to two allowed
CPUs, uses `tools/capped` with 2 GiB, and measures only one process at a time.
The checked-in `workload.json` is the workload contract. `workload.hir` computes
transfer data; `main.hir` adapts it to today's store API and imports the original
ledger-service's table operations. Each group is one run of the `transfers` transaction entry:
`store.transact` on a store the process opens (local), or `runner.transact` on a `db` runner whose
place serves the same entry (served). Port that adapter when the store API changes.

Eight accounts start with 1,000,000 units each. Transfer i moves 1 + i mod 3 units
from account i mod 8 to its successor, wrapping at 8. There are 512 successful
transfers per sample. Groups contain 1 or 8 transfers in one atomic transaction,
with one durable output gate per group. This is application batching, not eight
independent transactions sharing a store log group. Store `group_commits` is 1.
One, four, sixteen or sixty-four closed-loop clients each keep one group outstanding. Hirsel clients
are cooperative tasks on one execution worker and the Rust clients are threads, all feeding one writer.
Admission, channel queuing, commit, sync, and joining clients are included in
throughput. Latency starts at submission and ends when the writer returns from
the durable commit, before sending the acknowledgment. There is no open-loop
load or saturation claim. Both engines close and reopen before checking every
balance against a separately computed per-account oracle and checking total
conservation. The nonuniform amounts make the final balances nontrivial.

The served engine calls the same entry over `remote.tls@1` (the repository's TLS test
identities, crates/hirsel-rt/tests/it/tls-data) to a `hirsel serve --native` host. Its place's head
is `main.hir`'s `head`: one store over the place's `db` authority with the local mode's
configuration, serving the local mode's three entries. Served samples run in the `log-host` cell
only (`authority.log@1`, floor `host`, `group_commits` 1): the transport is the same at every level. The host links the head with the `run --native` preset, LLVM at the release optimization
level without scoped specialization. The caller runs interpreted, because native launch does not
offer `remote.tls@1` runners yet. A served commit's `instructions_commit`, `cpu_us_commit`, log
barriers and allocations are the head process's, counted by `perf stat` attached to it between the
caller's markers, with the same barrier check as the local engines. Its allocation diagnostic
probes the head's executable once it runs. `caller_*` and `host_*` give the caller's and the host's
own instructions and task clock. Throughput and latency are the caller's, so they include the
interpreted caller, TLS, the host's forwarding and the place link. A served store stays with its
place: the oracle runs as a served transaction, without a close and recovery. `results.json`'s
`served` gives each served cell's ratios to the local native and Rust cells of the same group and
submitters.

The Rust reference implements the same transfers and group/client counts with
an in-memory balance array and 24 little-endian bytes per transfer, one
`write_all` and `sync_all` per group. It pins the log's directory before timing.
The raw disk reference is a direct loop appending one byte and calling fsync.
Neither reference implements Hirsel's checksums, table catalog, recovery,
conditional object publication, fencing, or per-commit directory syncing.

`READY` and `DONE` delimit counters. The process waits for a host input at both
markers, so account setup, compilation, recovery checks and output serialization
are excluded. `perf stat -e instructions:u` counts the entire process tree during
this phase, including all submitter tasks/threads. The small perf-control handoff
and marker overhead is included in instructions, not in the application's clock.
Syscall tracepoints for pwrite64, fdatasync and fsync are enabled during native
and interpreter timing. Rust uses the forwarding fsync probe. Timing includes this observation overhead and the
interpreter's existing allocation meter overhead.

`alloc.c` forwards glibc malloc/calloc/realloc and fsync/fdatasync unchanged
in dynamic executables. Its counter deltas are read through
`/proc/<owned-child>/mem` while the process is at the gates. The shipping native
executable is static. Its allocator calls are counted in a separate diagnostic for each
workload using scoped uprobes on the binary's malloc/calloc/realloc symbols.
The runner registers uniquely named events with `sudo -n perf probe` and removes
only those events in a finally block. Diagnostic latency and throughput are
excluded from ratios. Native allocations count underlying libc allocator calls,
including runtime slab requests, not logical objects served from existing slabs.
The interpreter uses its existing mimalloc global allocation and reallocation
counters, read from the binary's local symbols, with `--mem-usage` enabling them.
Rust counts calls to its global System allocator. These are engine-specific
allocator requests, not a common logical-object metric; compare each engine with
its later runs. Aligned allocation APIs are not counted by the libc probes.

Bytes are the phase's increase in regular-file sizes, meaning encoded log or object bytes,
not block-device bytes or filesystem write amplification.

The `callers` engine is the served place with one caller process per submitter: 1, 4, 16 or
64 `hirsel run` processes, each with one client on its own `remote.tls@1` runner and so its own
call stream, against one `hirsel serve --native` host. Caller k commits the groups whose first
transfer is k * group modulo callers * group; caller 0 opens the accounts before the others
start and runs the oracle once all are done. The head can then see several arrivals in one
inbox batch, and `store.serve`'s window can let one barrier cover several commits, so the
barrier check asks for one `pwrite64` per commit and between one and `commits` fdatasyncs.
All callers run in one `tools/capped` scope of 8G (each process guards itself at 2G) on two
CPUs of their own, so the head and host keep the pair every other sample runs on. Throughput
is the runner's wall time from releasing every caller to the last caller's `DONE`; latencies are
every caller's. `commits_fsync` is commits per barrier, and `caller_cpu_us_commit` sums the
callers' CPU from `/proc`. A second, diagnostic sample probes the head's allocator calls and
records its `hirsel_rt_v2_inbox_take` and `hirsel_rt_v2_arrival_request` calls in order:
`inbox_batches` counts the takes some arrival followed, and `arrivals_batch` is arrivals per
such batch. `--engines`, `--groups` and `--submitters` pick subsets.

Each sample runs one cell: one authority at one level its provider declares, set as the
deployment floor, with `group_commits` 1, so each commit group is one advance. A cell
names the failure an acknowledged commit survives, and the runner refuses a native,
served or interpreter sample whose phase makes other kernel calls than the cell requires
(tracepoint counts; under `--profile`, the trace order too):

| Cell | Authority, level | Survives | Per commit group |
|---|---|---|---|
| `log-host` | `authority.log@1`, `host` | OS crash or power loss; the disk survives | one `pwrite64`, then one `fdatasync` |
| `log-process` | `authority.log@1`, `process` | process crash | one `pwrite64`, no sync |
| `memory` | `authority.memory@1`, `memory` | nothing outside the process | no file calls, no bytes |
| `objects-dir` | `authority.objects@1` over `objects.dir@1`, `host` | OS crash or power loss | at least one sync (each put syncs its file and directories) |

`--cells` picks a subset. The Rust and raw references run once per workload and pair
with `log-host`. Allocation diagnostics run for `log-host` only. No I/O tracing
is used in latency samples. Percentiles use nearest rank over 512 or 64 durable
groups; each transfer in a group shares its completion latency. These are one
sample's empirical percentiles, not confidence intervals or stable tail estimates.

`--profile` collects separate phase-gated CPU profiles and syscall-duration traces, retaining `owners.txt`
and `sync.trace`, and deleting raw stacks. Profiles are diagnostics and their times are not used
for throughput ratios. Symbols without line information need source-owner lookup.
The profile excludes the strace process; traced fsync durations attribute syscall
wall time separately from CPU samples. Strace overhead makes these diagnostic
timings unsuitable for throughput ratios. Fsync wall time is not CPU time. `result.json`, stdout with one latency per group,
perf counters, and stderr remain beside each database. `results.json` summarizes
the matrix. To resume the same unchanged build after a failed case, keep its
failed evidence elsewhere, remove just that incomplete case directory, and use
`--compiler <release-toolchain>/hirsel --skip-build --resume`; completed samples
are reused only when their saved source revision, full workload, engine, group,
submitter count and diagnostic mode match. Missing bindings and mismatches are
refused, including allocation, raw and profile cells. Legacy evidence stays intact;
use a fresh output directory to measure a changed revision or workload. The runner
requires committed source and keeps the captured revision in every cell and summary.
Do not resume across executable changes.

The published baseline was measured 2026-11-10 on
storage base `4722e8cb`, LLVM release, an AMD Ryzen 9 5950X, affinity 0-1, and
ext4 on local NVMe. Native achieved 1,645 / 1,893 transfers/s at group 1 and
10,490 / 10,374 at group 8 for 1 / 4 submitters. The interpreter achieved
49.6 / 55.4 and 70.5 / 75.1. Native throughput was 0.137-0.217 of the Rust
reference. Raw one-byte append-plus-fsync rate was 11,477/s. These are one-sample
baselines from a shared host, with empirical p99s, not performance thresholds.

| Transfers per commit | Submitters | Rust transfers/s | Native transfers/s | Interpreter transfers/s |
|---:|---:|---:|---:|---:|
| 1 | 1 | 8,767.59 | 1,644.56 | 49.63 |
| 1 | 4 | 8,713.79 | 1,892.85 | 55.39 |
| 8 | 1 | 62,848.54 | 10,490.26 | 70.54 |
| 8 | 4 | 75,858.84 | 10,373.63 | 75.09 |
