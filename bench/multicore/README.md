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
./bench/multicore/run.py --s0 \
  --build-cpus "$build_cpus" --measure-cpus "$measure_cpus" \
  --bins "$benchmark_bins" --out "$new_evidence_directory"
```

For Matmul scaling, use `--case matmul_t16 --workers 1 16` with the same
binaries and fixtures.

## Qualifying command

With the binaries built, run the following command and optionally select one
or more workloads with `--case` and widths with `--workers`. The default widths are 1, 2, 4, 8, 12 and 16.

```sh
./bench/multicore/run.py \
  --build-cpus "$build_cpus" --measure-cpus "$measure_cpus" \
  --bins "$benchmark_bins" --out "$new_evidence_directory"
```

Use the kernel-speedup table for shared Mandelbrot scaling. It times the
render after setup and ends before PBM output. The process-wall table and
instruction ratios still include output and startup. The runner validates
the whole PBM and retains its SHA-256 in the sample record. It saves the process-tree
`perf stat` counters, GNU time RSS, output and diagnostics outside the repository.
Set `build_cpus` and `measure_cpus` to CPU lists within your assignment.
Width N uses the first N measurement CPUs in the order supplied. The runner
refuses CPU assignments with a measurement CPU that shares a core with
another CPU in the same run. Each sample records its SMT siblings' activity.
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
./bench/multicore/run.py --standard-rust \
  --build-cpus "$build_cpus" --measure-cpus "$measure_cpus" \
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
measurements. The shipping archive uses one worker.

## S8 test pool

Build the release driver and `//:hirsel-core-rt-test-pool-archives`. Pass
the archive for your architecture to `build.py --s0 --runtime` and use
`run.py --test-pool` to select the runtime width. The runner retains raw
counters in each sample's stderr. For example, measure 200,000 transfers
from 16 producers and the nested spawn workload at width 16.

```sh
./bench/multicore/run.py --test-pool \
  --build-cpus "$build_cpus" --measure-cpus "$measure_cpus" \
  --bins "$benchmark_bins" --out "$new_evidence_directory" \
  --case fan_in fan_in_cap1 ping_pong ping_pong_burst spawn_chain \
  --workers 16 --languages Hirsel Tokio
```

The sequencer reports time holding its token and time from publication to
acquisition on queued hand-offs. It also counts the queued hand-offs whose predecessor
released on another L3 cache and their time. Divide their sums by token acquisitions
for cost per acquisition, and by twice the fan-in item count for cost per
channel commit including scheduler work. Channel wakes count waiting fibers
made runnable by sends and receives. The pool reports stolen queue entries
and vacant-slot transfers. Each worker reports its peak cached vacancies
and the number it returns at exit. Add `--s0` with `matmul_t16` or
`arc_pair` for the Rust comparisons. Matmul reports first dispatch count and
total submit-to-dispatch time per worker in both runtimes.

## S8u search fixtures

Build the release driver with `//:hirsel-core-rt-test-pool-archives` and pass
the test-pool archive for the host architecture. The runner sets the
runtime width and takes one capped sample for each side at widths 1 and 16.

```sh
taskset -c 0-11,16-27 python3 bench/multicore/build.py --s8u --out "$benchmark_bins" \
  --hirsel "$release_driver" --runtime "$test_pool_archive"
python3 bench/multicore/s8u.py \
  --build-cpus "$build_cpus" --measure-cpus "$measure_cpus" \
  --bins "$benchmark_bins" --out "$new_evidence_directory"
```

Knapsack splits at depth five and visits up to 2,048 nodes per live stage in
each round. The 60 seeded weights are in increasing order, with each value
equal to its weight plus 300. This gives density order for the fractional
upper bound. The portfolio uses four seeded single-bit searches with random
restarts on a 64-site chain. Each live stage searches up to 16,384 iterations
per round. The Hirsel owner submits to all live stages before reading each
capacity-one reply in lane order. The Rust region model keeps that round
structure and publishes updates inside a round via atomics. It uses Rayon
to model dispatch, private state and ordered collection. The Rust twins use
Rayon without rounds for knapsack and scoped threads for the portfolio.
The one-worker portfolio twin visits all four seeds in round-robin quanta.

The runner checks each returned witness against an independent
dynamic-programming optimum and reports explored work, relative speedups,
and cancellation lag. Knapsack reports the interval from discovery of the
final incumbent to the last search stopping. Hirsel stage clocks are
unavailable, so its owner timestamps bound the lag. The output receipt
records elapsed nanoseconds, work, quality, witness, lower and upper lag
bounds in nanoseconds, and rounds in that order.

## Scheduler grain sweep

For each regime, compare throughput at width 8 against that program's own
width-1 throughput at the same grain. Use the median of at least three
samples per deciding cell and report their spread.

- **collapses** if Hirsel has less than half its width-1 throughput and Tokio
  retains at least half its own width-1 throughput.
- **scales** if Tokio speeds up at least 2× and Hirsel reaches at least 0.6
  times Tokio's speedup.
- **flat** otherwise.

The crossover grain is the smallest sampled grain where a program at width 8
beats itself at width 1. Report the result for Hirsel, Tokio and Go where
available, including an absent or unmeasured crossover. Report the cell
verdicts and use the worst cell for the topology summary.

Apply these three conditions only with cost attributed to the ordering token
through waits, transfers or parks. Exclude defects and bounded local hot spots.

1. **(a)** The socket or checkbox service is slower at width 8 than width 1.
2. **(b)** At grain 10 µs or more a regime fails to scale where Tokio speeds
   up at least 4×.
3. **(c)** Hirsel's crossover exceeds ten times Tokio's in at least three
   topologies.

The kernel is xorshift64 with shifts 13, 7 and 17. The entire suite uses
the same iteration counts on each port and retains each port's observed grain.
Calibrate by timing 4,000 evaluations on one CPU, subtracting the zero-round
time and dividing by 4,000. The counts 0, 64, 640, 6,400 and 64,000 are the
calibrated inputs for approximately 0, 100 ns, 1 µs, 10 µs and 100 µs on the
Ryzen 9 5950X. Recalibrate once on a different host before the sweep. Calibration
uses seed values 1 through 4,000. Each benchmark prints the number of completed
units and the sum modulo 2^64 of the transformed inputs. The oracle computes
powers of the linear map over GF(2) and sums those powers applied to the
workload's seed interval. A wrong receipt fails the sample.

| Case | Unit and topology |
| --- | --- |
| `independent16`, `independent256` | One result per compute-only task. Join each wave before starting another. A wave has 16 or 256 tasks. |
| `pairs`, `single_pair` | One round trip. Each of 16 pairs, or the single pair, uses two capacity-one channels. The responder computes between receive and reply. |
| `fanin` | One item. Sixteen producers compute before sending to a capacity-64 queue with one collector. |
| `fanout` | One item. One dispatcher sends round-robin to 16 capacity-64 queues. Each worker computes and sends to a capacity-64 result queue. Hirsel receivers have a single owner, so every port uses the same dispatch. |
| `pipeline` | One item after eight capacity-64 stages. Each stage applies the kernel once. |
| `grain_spawn_join`, `grain_spawn_chain` | One joined child. One or 16 chains spawn then join each computing child before starting the next. |
| `spawn_tree` | One leaf. A depth-six binary tree spawns two children at each branch and joins both. |
| `channel_hub` | One exclusive permit. Sixteen clients compute, enter one capacity-one request queue, and receive the permit on a private channel. The broker waits for release before granting another permit. The broker owns the receivers. |
| `arc_hub` | One share and release on the same Arc. Sixteen tasks compute before each share and add the shared payload of 1 to the result. Go has no Arc twin. |
| `skew` | One hot round trip or idle wake. A single pair runs beside 1,000 one-shot timers. Grain applies to the pair. Report round trips/s during the hot pair's lifetime and the mean and maximum time from the idle deadline to resume. |
| `timers` | One wake from 10,000 one-shot timers staggered by task index. Each task takes its own clock reading, sleeps until that reading plus 2 ms and 1 µs per index, then computes. |
| `sockets64`, `sockets1024` | One TCP request. Each connection sends a little-endian seed and gets the transformed u64 back. A disjoint Tokio generator connects all sockets before releasing the closed-loop request stream. Report request-phase throughput and nearest-rank p50/p99 round trip latency. The server and generator verify the same receipt. |
| `checkbox` | One Returned toggle under `wire-load converge` with 64 clients, 200 toggles per client and pace 1,000. Grain stays native to the checkbox application. The checksum is the sum of bytes of the final board. The loader checks each subscriber and the fresh snapshot against the XOR oracle and the runner checks the receipt against the prescribed input bits. |

Use the release compiler and testing archive from the build below. The same
archive runs untaped at all widths and reports pool counters when width exceeds
one. Recorded pairs, fan-in, pipeline and spawn cases use a recording build
(`hirsel build --recording` / `hirsel run --record` and `hirsel replay`). With
the recording build, width 1 writes chooser 0 and widths above 1 write chooser
2. The Tokio comparison remains untaped. The checkout's checkbox entry has no
graceful completion for sealing a recording, so the service row runs untaped.

```sh
taskset -c "$build_cpus" python3 tools/buck2/prepare.py
taskset -c "$build_cpus" .buck2/bin/buck2 build -j2 \
  --target-platforms //tools/buck2:release --show-full-output \
  //:hirsel-core-driver-bin-hirsel //:hirsel-core-rt-test-pool-archives
taskset -c "$build_cpus" ./bench/multicore/build.py --regimes \
  --out "$benchmark_bins" --hirsel "$release_driver" --runtime "$test_pool_archive" --go "$go"

# Use the lane's assigned values. Build and measurement run sequentially.
rounds=(0 64 640 6400 64000)
taskset -c "$measure_cpus" ./bench/multicore/run.py --regimes \
  --case calibrate --workers 1 --size 4000 --repeats 1 --rounds "${rounds[@]}" \
  --bins "$benchmark_bins" --out "$calibration_dir" --lock "$lane_lock" \
  --build-cpus "$build_cpus" --measure-cpus "$measure_cpus"

for mode in untaped recorded; do
  taskset -c "$assigned_cpus" ./bench/multicore/run.py --regimes --mode "$mode" \
    --workers "${workers[@]}" --rounds "${rounds[@]}" --bins "$benchmark_bins" \
    --build-cpus "$build_cpus" --measure-cpus "$measure_cpus" --load-cpus "$load_cpus" \
    --lock "$lane_lock" --out "$evidence_dir/$mode"
done
./bench/multicore/run.py --regimes --read "$evidence_dir"/{untaped,recorded}/samples.json \
  --crossover-width 8

# Select the three measured cases with the lowest width-8 speedup.
taskset -c "$assigned_cpus" ./bench/multicore/run.py --regimes --profile --languages Hirsel \
  --case "$case" --grain-ns "$grain" --rounds "$round_count" --workers 8 --repeats 1 \
  --bins "$benchmark_bins" --out "$profile_dir" --lock "$lane_lock" \
  --build-cpus "$build_cpus" --measure-cpus "$measure_cpus" --load-cpus "$load_cpus"
```

Each repetition runs Hirsel followed by Tokio and then Go where available,
under one lock. The default repeats widths 1, 4 and 8 three times and other
widths once. Report measured SMT activity. Use assigned physical cores for
both server and loader. For the socket rows throughput excludes connection
setup. For checkbox it spans the convergence driver's lifetime. Process-tree
wall time, user instructions and RSS are retained independently. For live
services these count the server process tree and therefore exclude the load
generator. The sequencer counters report service time under the token and
publication-to-acquisition time for queued hand-offs. The hand-off interval
excludes any wait before publication. Acquisitions per explicit workload
operation use the source-level operation count, since the testing archive
has no total for implicit charge, refund, dispatch and cleanup events.

For a guarded block, select its cases with `--case` and pass
`--block-process-check cc1plus`. The runner holds the named lock across the
whole invocation and uses `pgrep -x` at both boundaries. It saves process
IDs, load averages and timestamps to `block.json`. Exit 75 means the block
must be repeated in a new output directory. A busy opening check starts no
samples. The lock closes when the invocation ends.

Use `--also-lock "$second_lock"` when the assigned CPUs require a second
lock. Both locks cover the process checks and the samples. Use
`--allow-smt` only when the assigned layout deliberately puts measured
threads on both SMT siblings or puts the load generator on a server
sibling or CPU. Keep both ports on the same layout and label results that
share the server CPUs. `--cycles` adds user cycles to the process-tree
counters.
