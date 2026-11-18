# Multicore benchmarks

Build from the repository root after `python3 tools/buck2/prepare.py`. Use the
release driver and shipping archive paths printed by the following command.
Build Goose at revision `d5ae5bbda990923fa4129f969c7f853c20d4d1e6` with a real C
compiler. The Go reference used Go 1.26.4. The Rust reference uses the standalone
benchmark crate's pinned Rust 1.99.0 and its lockfile.

```sh
taskset -c 0-11,16-27 .buck2/bin/buck2 build \
  --target-platforms //tools/buck2:release --show-full-output \
  //:hirsel-core-driver-bin-hirsel //:hirsel-core-rt-archives

git clone https://salsa.debian.org/benchmarksgame-team/benchmarksgame.git /tmp/benchmarks-game
taskset -c 0-1 ./bench/multicore/build.py --out "$benchmark_bins" \
  --game /tmp/benchmarks-game --hirsel "$release_driver" \
  --runtime "$shipping_archive" --go /path/to/go --goose /path/to/goose
```

`build.py` extracts the Program-page source at Benchmarks Game revision
`40296663ed350d5fe4a6ab5e367bab61cb77c219`. Each row uses the fastest listed
parallel version eligible for the named runtime. The selected ports use
parallel work already present upstream, with the worker controls below.
The Rust compiler uses the upstream Ivy Bridge target and Go uses
`GOAMD64=v2`. The source contributors and license are on each program page.

| Workload | Rust | Go | Shared input | Rust vectorization |
| --- | --- | --- | --- | --- |
| Mandelbrot | #4, Rayon | #4, workers from `GOMAXPROCS` | 16000 × 16000 PBM | Eight-lane source, LLVM emits AVX |
| Spectral norm | #6, Rayon | #4, workers from `GOMAXPROCS` | 1600 rows, 10 power iterations | Explicit SSE2/SSE3 intrinsics |
| Fannkuch-redux | #6, Rayon | #3, runtime honors `GOMAXPROCS` | 11 elements | Explicit SSSE3/SSE4.1 intrinsics |
| K-nucleotide | #3, same per-length hash kernels in a Rayon pool | #7, workers from `GOMAXPROCS` | FASTA Go #1 at 120000, record THREE | Scalar source |
| Binary trees | #5, Rayon | #2, goroutines | Depth 16 | Scalar source |

The k-nucleotide Rust submission used fixed OS threads. The Rayon adaptation
keeps each per-length hash job and lets the gate control the pool width. The
higher-ranked Rust #7 uses the retired Tokio thread pool. The higher-ranked
spectral-norm Go #8/#1 and binary-trees Go #6 run serially. Mandelbrot Go #4
retains two logical goroutines per worker. Rayon uses `RAYON_NUM_THREADS`.
[Benchmarks Game rankings](https://salsa.debian.org/benchmarksgame-team/benchmarksgame/-/blob/40296663ed350d5fe4a6ab5e367bab61cb77c219/public/fastest/rust-go.html)

The runtime programs use the existing crate's Tokio multi-thread builder and
the standard Go runtime. Hirsel runs with a 256 MiB logical root for
admitted work and uses the public task, channel, and collection interfaces.
The throughput cases keep a fixed 16 chains, yielding tasks, or producers.
Ping-pong uses a single request and reply in flight. Hirsel and Tokio use two
capacity-one queues. Go uses unbuffered channels. Fan-in uses a capacity-64
queue. Goose has native OS threads and process-wide typed queues. Its
bounded fan-in retries on full with the OS scheduler's yield. Hirsel and
Goose lack a public cooperative task-yield operation, so `yield_many`
runs on Tokio and Go. [Hirsel task and channel spec](../../docs/spec/tasks.md) · [Tokio scheduler benches](https://github.com/tokio-rs/tokio/blob/5f7be0ac42cb3e1b739da1562f98a797cd55a606/benches/rt_multi_threaded.rs) · [Tokio bounded contention](https://github.com/tokio-rs/tokio/blob/5f7be0ac42cb3e1b739da1562f98a797cd55a606/benches/sync_mpsc.rs) · [Goose threads and queues](https://github.com/aardappel/goose/blob/d5ae5bbda990923fa4129f969c7f853c20d4d1e6/docs/reference.md#22-threads)

## S0 fixtures

Matmul uses the Rayon twin so the worker count can change while all 16
row blocks and their kernels stay fixed. Both sides use the LangArena
matched Matmul at size 256. Each sets up the input matrices once,
transposes the right matrix once per iteration, computes and retains every
row block, joins in reverse submission order and checksums the center.
T1 is the T16 workload at width 1. Rust uses one Rayon worker at that
width. The Hirsel runtime currently executes the tasks on one worker.

The other one-worker receipts cover spawn and join, a channel round trip
with two capacity-one queues, and a single-owner Arc share/release pair.
Spawn and channel use Tokio's multi-thread runtime with one worker. The Arc
reference uses Rust std on the calling thread. Each executable prints its
completed iterations and checksum. Both fields must match the hand-derived
oracle independently on each side.

```sh
taskset -c 0-1 ./bench/multicore/build.py --s0 --out "$benchmark_bins" \
  --hirsel "$release_driver" --runtime "$shipping_archive"
taskset -c 0-11,16-27 ./bench/multicore/run.py --s0 \
  --bins "$benchmark_bins" --out "$new_evidence_directory"
```

For Matmul scaling, use `--case matmul_t16 --workers 1 16` with the same
binaries and fixtures.

## Qualifying command

With the binaries built, run the following command and optionally select one
or more workloads with `--case` and widths with `--workers`. The default widths are 1, 2, 4, 8, 12 and 16.

```sh
taskset -c 0-11,16-27 ./bench/multicore/run.py \
  --bins "$benchmark_bins" --out "$new_evidence_directory"
```

Use the kernel-speedup table for shared Mandelbrot scaling. It times the
render after setup and ends before PBM output. The process-wall table and
instruction ratios still include output and startup. The runner validates
the whole PBM and retains its SHA-256 in the sample record. It saves the process-tree
`perf stat` counters, GNU time RSS, output and diagnostics outside the repository.
Width N uses the first N of `0-11,16-19`. On the measured Ryzen 9 5950X that
means 12 distinct cores at width 16, four with both SMT siblings active.
Each point runs under `tools/capped` at 2 GiB with the matching CPU quota.
Startup is included. Each output must match its workload oracle. K-nucleotide
checks each frequency and exact query count. It allows either order for
equal counts and the exact float32 or float64 rounding at three decimal
places. A required missing or wrong sample returns failure. Use
`run.py --read path/to/samples.json` to print the tables again.

To measure the Rust reference at all five published sizes, use a separate
build directory. This keeps the FASTA input at n=25000000 distinct
from the compact shared FASTA fixture.

```sh
taskset -c 0-1 ./bench/multicore/build.py --standard-rust \
  --out "$rust_scale_bins" --game /tmp/benchmarks-game
taskset -c 0-11,16-27 ./bench/multicore/run.py --standard-rust \
  --bins "$rust_scale_bins" --out "$new_evidence_directory"
```

The standard Rust measurements use Mandelbrot 16000, spectral norm 5500,
fannkuch 12, FASTA 25000000 for k-nucleotide, and tree depth 21. The kernel
timer starts after pool initialization and any input parsing. For binary
trees it times the parallel batches. For k-nucleotide it times the seven
count jobs. Use the process-wall table for full lifecycle cost. Use the
shared suite for cross-language comparisons at identical inputs.

| Workload | Multicore slice measurement |
| --- | --- |
| `spawn_join` | S0 reference, S1 stack-reuse elapsed and instructions, S2/S3 instruction budgets, S6/S8 regressions, S9 runtime comparison |
| `ping_pong` | S0 round trip, S3 event overhead, S5 bounded-channel instructions, S6/S8 regressions, S9 runtime comparison |
| `spawn_chain`, `fan_in` | S8/S9 scheduler and shared-queue scaling. These extend the single-operation rows to 16 active lanes. |
| `yield_many` | S8/S9 peer scheduler scaling reference until Hirsel exposes a cooperative yield. |
| `mandelbrot`, `spectralnorm`, `fannkuchredux`, `knucleotide`, `binarytrees` | S8 test-pool and S9 cutover scaling, with independent work and reductions. The data rows extend the existing Matmul scaling gate. |

For S1's current-thread Tokio threshold, use the existing current-thread
reference. Arc, Matmul, runner throughput and replay use their dedicated
measurements. In today's Hirsel build, width changes the affinity of a
single-worker runtime. At S8 the caller must supply the test-pool build.
At S9 the runtime can use the selected affinity to choose its worker count.
