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
predecessor sets, and both Hirsel variants recurse in the DFS as upstream does.
Goose uses pools for data whose lifetime spans the traversal. Json
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
python3 bench/awfy/awfy_measure.py "$out/build" "$out/awfy" --upstream "$awfy" \
  --build-cpus "$build_cpus" --measure-cpus "$measure_cpus"
python3 bench/awfy/profiles.py "$out/build" "$out/awfy/samples.json" \
  "$out/profiles" --upstream "$awfy" --build-cpus "$build_cpus" --measure-cpus "$measure_cpus"
```

Each AWFY execution selects one benchmark by name and takes an outer iteration
count and an inner size. A sample runs one outer iteration at the size in
`rebench.conf`. For fixed-input workloads the sampler also supplies the
reference data to all three ports as a fourth argument. Queens reads a
cyclic sequence of board sizes, with one digit per search and ten searches
per inner iteration. The board uses `88888888`. The other numeric workloads
read comma-separated values, one per inner iteration. Json and JsonPool
receive the upstream RAP document as text.

The runtime sample counts process-tree user instructions and
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
Run `awfy_measure.py "$out/build" "$out/size-board" --upstream "$awfy" --size-board`
on the lane's assigned CPUs. It verifies the same 14 names, inputs and expected
results on all three harnesses, then uses GNU `strip --strip-all -R .comment` on each complete
executable. Report one Hirsel/Rust ratio and one Hirsel/Goose ratio for the
harnesses. Each ratio uses a single pair of binaries. A per-row size geomean
requires separately built, matched standalone executables. Report pool and
index variants separately.

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
python3 bench/awfy/awfy_measure.py "$out/build" "$out/pools-before" \
  --build-cpus "$build_cpus" --measure-cpus "$measure_cpus" --upstream "$awfy" --pool
```

To test the MEM-32 reopen condition, rebuild the same Hirsel harness with
the toolchain that meets the MEM-32 targets. Point the measurement
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
python3 bench/awfy/awfy_measure.py "$out/arena" "$out/arena-startup" --arena \
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
the order reversed on alternate rows. LangArena runtime uses the byte-for-byte
upstream run.js and verifies its checksums. A checksum mismatch fails the row.
Elapsed seconds cover the whole process on every side, including startup,
setup, warmups, measured iterations, verification and exit. The Startup rows
report Bounce, pool List and LangArena CLBG::Nbody with one iteration and zero
warmups. LangArena startup and compile verification use the one-iteration
Rust oracle.

Hirsel checks bounds and overflow, admits bytes and charges Steps only where a step bound can be current; Rust and Goose check bounds only.

The size row reuses verification of all 14 original workloads. The cold and edit compile rows compare the whole
LangArena suite against Cargo. The warm build changes Spectralnorm's checksum
from 1051192944 to 1051192945. It must compile at least one Hirsel part and
rebuild the Cargo benchmark crate. Both executables must pass the new checksum
fixture. Each Hirsel build must take its release std from the std product's
prebuild (`std-product --target`), as a build over the toolchain bundle does:
a build that lowers std itself (`native.reach.std_lowered` above zero) fails
its row as `std-lowered`. The checkbox rows compare the same wire load on a fresh private Hirsel
server and the Rust reference, using an ephemeral port.

Use at least two measurement CPUs. The first runs AWFY, LangArena and the
checkbox server. The second runs the checkbox loader. A measurement CPU
must have a core to itself within the run's CPU assignment. The board
reports sibling activity for each sample. The default lock is
`/run/user/<uid>/hirsel-board.lock` for the current user. On the timing host,
the board and cycle-pairs runner hold both shared timing locks, move into
transient system scopes, and reserve CPUs 0-3 and 16-19 from `user.slice`
and `system.slice` until the hold ends. The board needs Linux perf user
counters, CMake, Clang, Rust 1.99.0 with the pinned Cargo dependencies
available, Node.js, and GNU strip and time. Keep `board.json` and `board.md`
with the captured logs. Add `--baseline PREVIOUS/board.json` to check each
nightly gate.

Set `HIRSEL_BOARD_CARGO_ENV=NAME=VALUE` if your Cargo wrapper needs an
environment setting to stay inside the board's existing resource limits.

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

## Screen two revisions on Boat

The screen checks one Hirsel workload per runtime family: AWFY Bounce,
AWFY pool/index List and LangArena CLBG::Nbody. It reports user instructions,
stripped executable bytes and peak RSS per workload for both revisions on
one VM. Every sample must pass the board's workload check.

Prepare each revision in a clean local checkout. Use the same pinned AWFY
and LangArena checkouts as the nightly board.

```sh
taskset -c 8-11 python3 bench/awfy/screen.py prepare "$prepared" --awfy "$awfy" --langarena "$langarena"
taskset -c 8-11 python3 bench/awfy/screen.py run "$out" --base "$base" --candidate "$candidate" --awfy "$awfy"
```

The screen prints the table and saves it in the Boat job logs under `$out`.
The nightly board supplies the full peer comparison.

## Run the on-demand comparisons

Use a clean candidate and the pinned peer checkouts. Fetch locked Cargo
dependencies before starting. The output directory must be new.

```sh
python3 bench/awfy/board.py "$out" --comparison \
  --build-cpus 8-11 --measure-cpus 16-19 \
  --awfy "$awfy" --resid "$resid" --goose "$goose" --langarena "$langarena" \
  --units-deadline-seconds "$units_budget"
```

Repeat `--comparison-family` to select among `compile`, `check`,
`runtime-memory`, `image-size`, `checkbox` and `units`. A selected run can
complete its requested rows. The full board requires all six families.
`comparison.json` holds raw samples, modes, revision and peer pins.
`comparison.md` shows raw values, ratios, coverage and refusal reasons.
Validate and render the evidence without building or measuring again with
`python3 bench/awfy/board.py render-comparison "$out" --revision "$commit"`.

Builds use up to four jobs with an 8 GiB allocation. The board measures
under the regime lock followed by the board lock, outside 02:45–04:00 local
time, with a 2 GiB cap on each measured process tree. CPU 16 runs serial
measurements. Checkbox uses CPUs 16 and 17 for server and loader. Cargo
runs inside the board's process-tree scope. Instructions and cycles count
user-space CPU for the whole measured tree. Cycles and elapsed seconds are
single observations.

| Row | Metric boundary and qualification |
| --- | --- |
| LangArena dev compile | Whole-suite Cranelift, Cargo dev and Goose debug C build/link. The one-iteration Spectralnorm checksum is 1,051,192,944 cold and 1,051,192,945 after a one-body edit. Hirsel starts with a published product and reports the combined app and library unit count and object writes to the published store. Goose rebuilds the suite on the edit. |
| LangArena check | The same staged application and edit, with dedicated check stores. Hirsel must check application bodies and accept the entry and source manifest. Cargo must produce fresh application metadata. Goose checks the suite and its imports in full on both passes. |
| LangArena release compile | Whole-suite LLVM, Cargo release and Goose emission plus Clang compile/link, with the same Spectralnorm oracles. |
| AWFY original-14 release compile | Original harness only, with the board's LLVM and Goose/Clang flags and an isolated dependency-free Cargo package for the Rust port. Each executable must pass all 14 reference workloads. The edit adds an exact `board-edit` marker to the verified-success line. |
| Peak RSS | Whole-process peak in KiB from the verified runtime samples. The suite ratios cover 14/14, 8/8 and 50/50 pairs for each peer. The aggregate geomean uses all 72 underlying ratios once per peer. Missing or censored pairs reduce coverage and withhold that peer's aggregate. |
| Checkbox memory | Fresh private stores with the same verified seed and 64 live connections per arm. Each subscription mode reports live `smaps_rollup` RSS and process `VmHWM`, both in bytes. Rust is the server peer. |
| AWFY image size | Bytes after GNU `strip --strip-all -R .comment` on a disposable copy of each verified original-14 harness. There is one image per peer. |
| Units A1 | Hirsel/Tokio passing offered ceilings, measured goodput and scheduled-arrival latency by filesystem, population and SHA-256 backend. Matched offers separately compare CPU µs per verified turn and latency with the same offered rate and qualified sync grouping. |
| Units regimes | Hirsel/Tokio work rates and relative width-1 speedups at each verified case, grain and worker width. Checkbox has native grain. Arc and Checkbox expose one runnable unit. |

Compile and check receipts include the command, dedicated cache mode, active
PMU counters, elapsed seconds and peak RSS. The edited Hirsel and Cargo arms
reuse their own cold application state. Cargo primes dependencies before
measurement and clears application artifacts. Goose performs a full build
or check for the edit.

The units owner runs outside the board's lock holds. Supply
`--units-receipt /path/to/fair.json` with the producer's raw pair and trial
files beside it. The board requires a full same-revision export with the
exact cell inventory, an eight-worker topology, the 2 GiB measurement cap,
the build allocation and the common durable sync policy. The receipt
config records those limits as `measurement_memory_limit_bytes`,
`build_memory_limit_bytes`, `build_cpus`, `build_jobs` and `sync_policy`.
Without an import, the board invokes the same-revision producer when the
units owner authorizes its separate host allocation and caps with
`HIRSEL_BOARD_UNITS_AUTHORIZED=1`. Missing owner evidence leaves the affected
cells pending. Only an ordinary nightly run supplies a promotion receipt.

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

When std sources change, pass `--whole-std` to both release build adapters.
Each arm builds an explicit whole-library image from its own source manifest.
Every release build lowers only the std roots its program reaches, whether
the std comes from an image or from sources. For a source-only comparison,
pass `--source-std` to both native build commands and give each arm a fresh
cache.

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
execution argv and CPU affinity, as well as build flags and the
release compiler, runtime, source, image and fixture hashes. When candidate
identity changes, both rows must carry verified build records. The report
names the changed values.
Identical candidate identities across all rows cause an A/A refusal. Use
`compare --aa` for a deliberate run with identical candidate identities.
Each LangArena image's workload source hash covers its benchmark group and the
shared harness and helper. Its fixture hash covers the configuration file's
bytes. AWFY's JSON rows include the generated JSON payload in their fixture
hash. A changed fixture refuses comparison even when the output still matches.
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
  --rows awfy/Mandelbrot --cpu "$cpu" --lock "$lock"
```

Each pair runs both images in random order on the measurement CPU and
samples SMT sibling activity. The default uses all 60 pairs. With
`--sibling-busy-threshold PERCENT`, keep a pair only when both runs record
less than that busy percentage on every SMT sibling. Set `--pairs` and
`--max-attempts` to control the sample size and attempt budget. Use `--aa`
to measure identical builds deliberately. The command exits 0 if every
row has enough pairs and its median cycle ratio is at most 1.03x, 1 if a
row exceeds that ratio, or 2 if a row has too few pairs or the comparison
is refused.

To measure against Rust and Goose, pass a single Hirsel receipt directory.
Put the peer binaries beside the Hirsel image using the normal build layout.
Add `--upstream "$awfy"` for AWFY or pool peer commands.

```sh
python3 bench/awfy/receipt.py cycle-pairs "$out/candidate-samples" \
  --peer rust goose --rows 'arena/Sort::Merge' --cpu "$cpu" --lock "$lock"
```

Peer mode checks all outputs and reports Hirsel/peer ratios. Exit 0 means all
requested pairs completed. The ratios have no regression threshold.

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
where instruction work occurs. The innermost recognized frame names the
phase, so the pre-LLVM phases (`std-load`, `code-store`, `request-store`,
`tool-inputs`) take their hashing and copying away from `reach` and `other`.

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
