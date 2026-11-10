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
ledger-service's table operations. Each group is one `store.transact` entry run; runner transport and
`store.call` stream replay handling are excluded. Port that adapter when the store API changes.

Eight accounts start with 1,000,000 units each. Transfer i moves 1 + i mod 3 units
from account i mod 8 to its successor, wrapping at 8. There are 512 successful
transfers per sample. Groups contain 1 or 8 transfers in one atomic transaction,
with one durable output gate per group. This is application batching, not eight
independent transactions sharing a store log group. Store `group_limit` is 1.
One or four closed-loop clients each keep one group outstanding. Hirsel clients
are cooperative tasks on one execution worker and the Rust clients are threads, all feeding one writer.
Admission, channel queuing, commit, sync, and joining clients are included in
throughput. Latency starts at submission and ends when the writer returns from
the durable commit, before sending the acknowledgment. There is no open-loop
load or saturation claim. Both engines close and reopen before checking every
balance against a separately computed per-account oracle and checking total
conservation. The nonuniform amounts make the final balances nontrivial.

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
Sync/publication tracepoints are enabled during native timing. Other engines use
the forwarding fsync probe. Timing includes this observation overhead and the
interpreter's existing allocation meter overhead.

`alloc.c` forwards glibc malloc/calloc/realloc, fsync/fdatasync and linkat unchanged
in dynamic executables. Its counter deltas are read through
`/proc/<owned-child>/mem` while the process is at the gates. The shipping native
executable is static. Its baseline sync/publication counts use kernel syscall
tracepoints. Its allocator calls are counted in a separate diagnostic for each
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

Bytes are the phase's increase in regular-file sizes, meaning encoded log bytes,
not block-device bytes or filesystem write amplification. Backend requests count
conditional log-object publication attempts via linkat; today's steady writer
issues only `Backend.put_new`. References count one append per group. File and directory sync calls are counted separately; successful transaction
completion checks the barrier outcomes. No I/O tracing
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
