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
share one suite binary per language. Each Hello uses a separate binary.

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
standard Resid ports of the same workloads.

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
