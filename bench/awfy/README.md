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
Hirsel repo root, set `build_cpus` and `measure_cpus` to your assigned CPU
lists. Measure and profile use the first measurement CPU in the list.

```sh
taskset -c "$build_cpus" python3 tools/buck2/prepare.py
taskset -c "$build_cpus" python3 core/tools/measurement_artifacts.py > "$out/toolchain.json"
taskset -c "$build_cpus" python3 bench/awfy/build.py "$out/build" --upstream "$awfy" \
  --artifacts "$out/toolchain.json" --goose "$goose/build/goose" \
  --rust "$resid/bench/v2/t1-awfy/rust"
python3 bench/awfy/measure.py "$out/build" "$out/awfy" --upstream "$awfy" \
  --build-cpus "$build_cpus" --measure-cpus "$measure_cpus"
python3 bench/awfy/profiles.py "$out/build" "$out/awfy/samples.json" \
  "$out/profiles" --upstream "$awfy" --build-cpus "$build_cpus" --measure-cpus "$measure_cpus"
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
results on all three harnesses, then uses GNU `strip --strip-all -R .comment` on each complete
executable. Report one Hirsel/Rust ratio and one Hirsel/Goose ratio for the
harnesses. Each ratio uses a single pair of binaries. A per-row size geomean
requires separately built, matched standalone executables. Report pool and
index variants separately and state whether the compiler proved that Step
charging could be elided for each Hirsel image.

## Matched document pools

To build the matched pool/index harnesses, include `pool_index` in
`--hirsel-entries` and `--goose-entries` when invoking `build.py`. The `--rust`
option already builds `rust-pool_index`.

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
python3 bench/awfy/measure.py "$out/build" "$out/pools-before" \
  --build-cpus "$build_cpus" --measure-cpus "$measure_cpus" --upstream "$awfy" --pool
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
python3 bench/awfy/measure.py "$out/arena" "$out/arena-startup" --arena \
  --build-cpus "$build_cpus" --measure-cpus "$measure_cpus"
```

The startup configuration keeps every upstream input and sets one iteration
and zero warmup iterations. The Rust run supplies each first-iteration
checksum. Each sampled binary must confirm that checksum. Rust and Goose use
their full LangArena suite executables. Each Hirsel group uses its existing
standalone executable and root limit.

## Nightly goal board

From a dedicated checkout of main, run the board with clean source checkouts
at the four pins printed by `board.py --help`. The output directory must be
new and outside the checkout. The nightly host uses `build_cpus=20,21` and
`measure_cpus=10,11` and passes both lists explicitly. Use your lane's lists
for standalone measurements, checkbox memory, and multicore runs.

```sh
python3 bench/awfy/board.py "$out" \
  --build-cpus "$build_cpus" --measure-cpus "$measure_cpus" \
  --awfy "$awfy" --resid "$resid" --goose "$goose" --langarena "$langarena"
```

The board builds the release compiler and shipping runtime, builds fresh
peers, then samples AWFY original-14, AWFY pool/index (8), and LangArena
(50). Each runtime row runs adjacent Rust, Hirsel and Goose samples, with
the order reversed on alternate rows. The size row reuses verification of
all 14 original workloads. The cold and edit compile rows compare the whole
LangArena suite against Cargo. The warm build changes Spectralnorm's checksum
from 1051192944 to 1051192945. It must compile at least one Hirsel part and
rebuild the Cargo benchmark crate. Both executables must pass the new checksum
fixture. The checkbox rows compare the same wire load on a fresh private Hirsel
server and the Rust reference, using an ephemeral port.

Use at least two measurement CPUs. The first runs AWFY, LangArena and the
checkbox server. The second runs the checkbox loader. A measurement CPU
must have a core to itself within the run's CPU assignment. The board
reports sibling activity for each sample. It holds
`/run/user/1000/hirsel-board.lock` while measuring by default. It needs Linux perf user
counters, CMake, Clang, Rust 1.99.0 with the pinned Cargo dependencies
available, Node.js, and GNU strip and time. Keep `board.json` and `board.md`
with the captured logs. Add `--baseline PREVIOUS/board.json` to check each
nightly gate.

- The instruction gate flags a Hirsel increase greater than 2% on any row.
  Hirsel and peer CPU counters require equal enabled and running times.
  A refused row is reported alongside comparisons for the remaining rows.
- The size gate reports changes for each stripped original-14 harness and
  flags Hirsel growth of 2,048 bytes or more. The receipt includes the strip
  version and options and SHA-256 hashes of the stripped and original binaries.
- The checkbox gate flags RSS per live session growth greater than 10% on
  either server, for both subscribed and unsubscribed sessions. The board
  reports toggles/s and its change from the baseline as a trend.

Exit 0 means the board is complete and clean. Exit 1 means measurements are
incomplete, exit 2 means a comparison or counter was refused, and exit 3
means a gate reported a regression. A refusal takes precedence over a
regression. The wrapper promotes only exit 0. A refusal holds automatic
promotion until an explicit rebaseline.

When a workload or measurement changes, produce a complete run by leaving
out `--baseline` in the command above. Review that run, then promote it with:

```sh
python3 bench/awfy/board.py rebaseline "$out"
```

The command validates the run and prints its commit and the reason it was
promoted. It also accepts a clean wrapper run directory and updates
`~/.local/share/hirsel-board/last-complete.json` atomically.

## Qualify a runtime cycle claim

Run `receipt.py sample` with the base toolchain, its build directory, the
assigned `--cpu` and `--lock`, and `--pairs 7 --rows List,Havlak,Sieve,Storage,Json`.
This runs A/A on the same image. Repeat with the same base options and add
`--candidate-artifacts`, the corresponding `--candidate-awfy-build` (or
`--candidate-pool-build` or `--candidate-arena-build`), and `--aa` pointing
to the first run's `pairs.json`. Use a new output directory for each run.
Each checked pair runs back to back and reverses order on alternate trials.
The receipt records SMT sibling activity for each sample, and the runner
reports the median and min/max of the paired instruction and cycle changes.
An A/B median cycle change inside the matching A/A min/max is "within noise".
Each row prints the sibling's busy range. If activity differs by more than
20 percentage points between the two halves of any pair, the cycle verdict is
"sibling busy". Only the instruction threshold decides whether a comparable
run passes.

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
counters with equal enabled and running times. It checks the exact
execution argv and CPU affinity, as well as certification, build flags and the
release compiler, runtime, std/core source and fixture hashes. When the compiler,
runtime or std/core source changes, both rows must carry verified build records.
The report names the changed hashes. Identical candidate hashes across all
rows cause an A/A refusal. Use `compare --aa` for a deliberate run with
identical compiler, runtime and std/core hashes.
An identity refusal exits 2. A comparable
row above +0.5% exits 1. Use `--threshold PERCENT` to set another limit.

The same command reads two complete `board.json` files. It compares Hirsel
instructions on the 72 runtime rows and the cold and warm-edit compile rows.
The board uses this same comparator for its nightly +2% instruction gate.

### Qualify a cycle regression

Pass the directories containing the base and candidate `receipt.json` or
`board.json`, with the recorded images and fixtures still in place.

```sh
python3 bench/awfy/receipt.py cycle-pairs "$out/base-samples" "$out/candidate-samples" \
  --rows awfy/Mandelbrot --cpu "$cpu" --pairs 9 --max-attempts 60 --lock "$lock"
```

Each pair runs both images in random order on the measurement CPU. By
default, both runs must record less than 5% busy time on every SMT sibling.
Use `--sibling-busy-threshold` to change that limit and `--aa` to measure
identical builds deliberately. The command exits 0 if every row has enough
quiet pairs and its median cycle ratio is at most 1.01x, 1 if a row exceeds
that ratio, or 2 if a row has too few quiet pairs or the comparison is refused.

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

### Compare ELF size

Pass two unstripped images to `receipt.py elf-census BASELINE CANDIDATE`.
It emits JSON with section file bytes, sized symbols and their sections,
relocation and unwind bytes, and the bytes outside sections. It creates its own
temporary GNU-stripped copies for the shipping-size comparison.

### Profile a cold compile

Use `python3 bench/awfy/cold_phases.py OUT --artifacts TOOLCHAIN
--fixture ONE_ITERATION_JSON --cpus LANE_CPUS --lock LANE_LOCK` to profile on
your assigned CPUs. Setup uses the whole list and profiling uses the first
CPU. To supply separate lists, use `--build-cpus` and `--measure-cpus`.
The tool prepares the std product on the build CPUs, compiles the suite with
a fresh request store and checks
the Spectralnorm result. The receipt carries the compiler's stage metrics
and a separate breakdown of process-tree PMU instruction stack samples.
Entries in `other` have no recognized phase frame. The samples estimate
where instruction work occurs.

Add `--pass-trace --top 10` to count a fresh-store build with the LLVM
trace off and another with it on. Both builds use the same prebuilt std
product and must emit byte-identical images. The cold-phase receipt records
the process-tree instruction delta and each LLVM pipeline's counter total.
The compressed JSON Lines trace records the pass, IR unit, pipeline and
exclusive instructions for each instrumented invocation. LLVM's legacy
object emitter contributes a pipeline total. The unattributed column
includes pass-manager work between callbacks and trace bookkeeping.

Read a trace or rank the changes between two traces by absolute instruction
delta with the same command.

```sh
python3 bench/awfy/cold_phases.py report TRACE --top 10
python3 bench/awfy/cold_phases.py report AFTER --baseline BEFORE --top 10
```
