# Store measurement

`measure.hir` compares the work and retained snapshot size of `std@1/store`
loading fixed-size keys and values. It supports decisions about index growth,
compaction work, and conflict-tracker retention while a serializable transaction
remains open. It is a store workload, not a compiler benchmark or a throughput
claim for a deployed service.

Build once with the production LLVM toolchain:

```sh
hirsel build --release bench/store/hirsel.json -o /tmp/store-measure
```

For each invocation, create a new empty directory outside the repository and
supply a grants file with this shape, replacing `/tmp/store-data` with that
directory's absolute path:

```json
{"io":{"stdout":"stdout.bytes@1","dirs":[{"path":"data","host":"/tmp/store-data","access":"read_write"}]},"args":[]}
```

Run the same executable and key count for every compared revision:

```sh
HIRSEL_JOBS=1 HIRSEL_MEMORY_LIMIT=2G tools/capped \
  -- perf stat -e instructions:u -- \
  taskset -c "$BENCH_CPU" /tmp/store-measure \
  --hirsel-grants /tmp/store-grants.json compact 100
```

For a source/runtime smoke check, build with `--debug --backend cranelift`
and use the same four modes. Keep these results separate from production LLVM
cost comparisons.

Select an ordinary CPU allowed by the host. Start with 100 keys; every process
stays within 60 CPU seconds and 2 GiB. The program writes 16-byte keys and
100-byte values in transactions of at most 100 keys. Modes are `load`,
`compact` (one compaction after loading), `recompact` (compact, commit one more
key, compact again), and `begin` (hold a transaction across loading, then roll
it back). All modes close the store before printing total snapshot bytes.
A zero byte total is expected when no snapshot exists; this is not a work
counter. A nonzero exit refuses the sample.

Compare whole-process `instructions:u` and peak RSS externally, alongside the
printed snapshot bytes. Use three seeded interleaved rounds, a fresh empty
store for every cell, and retain the revision, mode, key count, grants, argv,
stdout/stderr and counters. Compare the same mode and key count across
revisions. To study scaling, retain the smaller and larger key counts as
separate cells. Startup, loading and close are included, so these totals do
not isolate a single store operation. There is no committed baseline or cost
gate yet; snapshot bytes alone cannot establish an index CPU or memory win.
