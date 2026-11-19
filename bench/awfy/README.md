# Are We Fast Yet

This directory contains native Hirsel and Goose ports of all 14 AWFY workloads.
The suite's reference inputs, verification values and standard inner iteration
counts come from AWFY revision `f35aed0a61171594804a44efe9d75f1f35360700`.
Rust uses the complete community port in Resid revision
`0ab89f373829776d3ea285bfb25f3f1505d7be35`. The Goose compiler is
`d5ae5bbda990923fa4129f969c7f853c20d4d1e6`, the LangArena comparison pin.
[AWFY source](https://github.com/smarr/are-we-fast-yet/tree/f35aed0a61171594804a44efe9d75f1f35360700) ·
[Resid Rust port](https://github.com/larrydewey/Resid/tree/0ab89f373829776d3ea285bfb25f3f1505d7be35/bench/v2/t1-awfy/rust) ·
[Goose](https://github.com/aardappel/goose/tree/d5ae5bbda990923fa4129f969c7f853c20d4d1e6)

The ports follow the AWFY JavaScript algorithms. Richards and DeltaBlue use
variants for task and constraint dispatch. Graph links use indices into the
run's owning collections. Havlak keeps the suite's bucket dictionary and
predecessor sets. Hirsel uses explicit DFS frames to fit the native stack
limit. Goose uses pools for data whose lifetime spans the traversal. Json
builds a tree with the suite's parser and object hash index. The build extracts
the RAP document directly from upstream. CD uses the suite's red-black tree and
voxel traversal. Hirsel uses fdlibm's medium-range trigonometric kernels over
its floating-point API. Resid uses Rust's `VecDeque` for DeltaBlue and Havlak
work lists and `HashMap` for Havlak's block numbering. These collection choices
are part of the comparison.
[AWFY guidelines](https://github.com/smarr/are-we-fast-yet/blob/f35aed0a61171594804a44efe9d75f1f35360700/docs/guidelines.md) ·
[Third-party notices](NOTICES.md)

## Build and sample

Use checkouts of the pinned AWFY and Resid revisions, and a Release CMake
build of the pinned Goose compiler. Set `out` to a directory outside this repo
and point `awfy`, `resid` and `goose` at the respective checkout roots. From the
Hirsel repo root:

```sh
taskset -c 0-11,16-27 python3 tools/buck2/prepare.py
taskset -c 0-11,16-27 python3 core/tools/measurement_artifacts.py > "$out/toolchain.json"
taskset -c 16-17 python3 bench/awfy/build.py "$out/build" --upstream "$awfy" \
  --artifacts "$out/toolchain.json" --goose "$goose/build/goose" \
  --rust "$resid/bench/v2/t1-awfy/rust"
taskset -c 16 python3 bench/awfy/measure.py "$out/build" "$out/awfy" --upstream "$awfy"
taskset -c 16 python3 bench/awfy/profiles.py "$out/build" "$out/awfy/samples.json" \
  "$out/profiles" --upstream "$awfy"
```

Each AWFY execution selects one benchmark by name and takes an outer iteration
count and an inner size. A sample runs one outer iteration at the size in
`rebench.conf`. The runtime sample counts process-tree user instructions and
takes peak RSS from GNU time. The startup sample measures monotonic elapsed time
from the spawn request to the first line confirming the work. For Hello that
line is the greeting. For AWFY it follows one complete checked outer iteration.
GNU strip reports the size of the full executable after stripping. AWFY tasks
share one suite binary per language. The Hirsel harnesses reach each workload's entry through a
function value, as Rust's harness calls a separate function per benchmark, so
no workload is compiled into the dispatcher and rows do not share register
allocation. Each Hello uses a separate binary.

## Original AWFY size board

Build into a fresh output directory with the three release toolchains above.
Run `measure.py "$out/build" "$out/size-board" --upstream "$awfy" --size-board`
on the lane's assigned CPUs. It verifies the same 14 names, inputs and expected
results on all three harnesses, then uses GNU `strip --strip-all` on each complete
executable. Report one Hirsel/Rust ratio and one Hirsel/Goose ratio for the
harnesses. Each ratio uses a single pair of binaries. A per-row size geomean
requires separately built, matched standalone executables. Report pool and
index variants separately and state whether the compiler proved that Step
charging could be elided for each Hirsel image.

## Matched document pools

The separate Hirsel and Goose pool harnesses accept `JsonPool` and `StoragePool`. Both use
one document owner per inner iteration. Their append-only collections hold
fixed-size descriptors and typed, one-based links. Links never leave the
document's construction and verification, and collection indexing checks
bounds. Storage reserves each node's descriptor and contiguous run of child
slots before descending. Json preserves the AWFY parser and object hash
index. It stores unescaped text and number spans as input offsets and puts
only decoded strings in a shared byte buffer.

Build the harnesses as above, then sample the pool cases at the same AWFY
sizes. Each Hirsel and Goose run must report `verified checksum=156` for
Json and `verified checksum=5461` for Storage. These are the AWFY reference
operations length and tree node count. The Rust denominator uses the
matched ports in `pool-ports/rust`, compiled separately from the original
Resid harness. Do not use original `Json` or `Storage` as pool denominators.

```sh
taskset -c 0-11,16-27 python3 bench/awfy/measure.py "$out/build" "$out/pools-before" \
  --upstream "$awfy" --pool
```

To test the MEM-32 reopen condition, rebuild the same Hirsel harness with
the toolchain that implements all MEM-32 obligations. Point the measurement
at its build directory and pass `--pool --sides hirsel`. Reuse the first
Goose and Rust samples as denominators. Compare Hirsel/Goose for each workload
against the 1.5× threshold in [MEM-32's reopen rule](../../docs/spec/memory.md#memory-model-settled).

## LangArena

Use the upstream Rust and Goose release suite binaries from LangArena revision
`3c9f84ddaeec834c0631747cb789e8cfd602f993` and Goose revision
`d5ae5bbda990923fa4129f969c7f853c20d4d1e6`. Copy those binaries to the
`arena` output directory as `rust` and `goose`. Build and sample the standard
Hirsel group entry points with the same configuration:

```sh
taskset -c 16-17 python3 bench/awfy/arena.py "$out/arena" \
  --config core/langarena/rust/run.js --artifacts "$out/toolchain.json" --rust "$out/arena/rust"
taskset -c 16 python3 bench/awfy/measure.py "$out/arena" "$out/arena-startup" --arena
```

The startup configuration keeps every upstream input and sets one iteration
and zero warmup iterations. The Rust run supplies each first-iteration
checksum. Each sampled binary must confirm that checksum. Rust and Goose use
their full LangArena suite executables. Each Hirsel group uses its existing
standalone executable and root limit.

## Nightly goal board

From a dedicated checkout of main, run the board with clean source checkouts
at the four pins printed by `board.py --help`. The output directory must be
new and outside the checkout.

```sh
taskset -c 20-21 python3 bench/awfy/board.py "$out" \
  --awfy "$awfy" --resid "$resid" --goose "$goose" --langarena "$langarena"
```

The board builds the release compiler and shipping runtime, builds fresh
peers, then samples AWFY original-14, AWFY pool/index (8), and LangArena
(50). Each runtime row runs adjacent Rust, Hirsel and Goose samples, with
the order reversed on alternate rows. The size row reuses verification of
all 14 original workloads. The cold and edit compile rows compare the whole
LangArena suite against Cargo. The checkbox rows compare the same wire load
against a fresh private Hirsel server and the Rust reference, never port 7700.

The board uses CPUs 20–21 for builds and 10–11 for samples. It holds
`/run/user/1000/hirsel-board.lock` while measuring. It needs Linux perf user
counters, CMake, Clang, Rust 1.99.0 with the pinned Cargo dependencies
available, Node.js, and GNU strip and time. Keep `board.json` and `board.md`
with the captured logs. An incomplete comparison has no ratio and makes the
command exit nonzero.

## Paired instruction guard

Use the release build adapters above, or `arena.py` with the Rust one-iteration
oracle. Run the sampler for each side with the same fixture, row selection, CPU
and lock. Paths to build directories may differ. Include `--pool-build` for the
eight matched pool/index rows and `--arena-build` for the 50 LangArena rows.
Omit `--only` to sample every row in the selected suites.

```sh
python3 bench/awfy/receipt.py sample "$out/base-samples" --artifacts "$base_toolchain" \
  --awfy-build "$base_build" --upstream "$awfy" --cpu "$cpu" --lock "$lock" \
  --only awfy/Mandelbrot
python3 bench/awfy/receipt.py sample "$out/candidate-samples" --artifacts "$candidate_toolchain" \
  --awfy-build "$candidate_build" --upstream "$awfy" --cpu "$cpu" --lock "$lock" \
  --only awfy/Mandelbrot
python3 bench/awfy/receipt.py compare "$out/base-samples/receipt.json" \
  "$out/candidate-samples/receipt.json"
```

The guard requires checked stdout, complete row coverage and instruction
counters active for at least 99% of their enabled time. It checks the exact
execution argv and CPU affinity, as well as certification, build flags and the
release compiler, runtime and fixture hashes. The build adapters record the
actual flags and image hash. A changed identity exits 2. A comparable
row above +0.5% exits 1. Use `--threshold PERCENT` to set another limit.

The same command reads two complete `board.json` files. It compares Hirsel
instructions on the 72 runtime rows and the cold and warm-edit compile rows.
`board.py --baseline PREVIOUS/board.json` records a nightly +2% triage result
and uses exit 3 for a comparable regression. An incomplete board exits 1.

### Profile a runtime delta

Build both images with `--debug-lines on` for authored and inlined Hirsel
locations. AWFY's build adapter already sets it. Pass `--debug-lines` to
`arena.py` for LangArena builds. Measure both images
with the sampling command above. Then use the same measurement CPUs and lock.

```sh
python3 bench/awfy/receipt.py profile-diff awfy/Mandelbrot \
  "$out/base-samples/receipt.json" "$out/candidate-samples/receipt.json" \
  "$out/mandelbrot-profile" --lock "$lock"
```

The command prints the uninstrumented PMU delta first, followed by the
Callgrind self-count deltas. It permits different release compiler or runtime
hashes for attribution and prints both hashes when they differ. Each side
keeps its raw profile, verified stdout and address map alongside offsets,
disassembly and DWARF inline frames when available.
