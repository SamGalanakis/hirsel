# Measurement protocol

Every ratio to Rust uses the same workload, input bytes, observable result, algorithm,
layout class, arithmetic policy, target, optimization and cache condition. Report
idiomatic Rust and representation-matched Rust separately. Pair Hirsel check-and-lower
with Rust check, end-to-end native build with Rust build, and executable lifecycle with
Rust run. Check is not a native-build denominator. A retained program-record hit is an
absolute startup number, separate from a fresh per-line compilation.

## Fixed configuration

Rust is 1.99.0, edition 2024, target `x86_64-unknown-linux-gnu`, target CPU `x86-64`.
The standard library is the toolchain's prebuilt library. Both profiles enable overflow
checks, disable incremental compilation and debug info, and use one codegen unit.
Dev uses opt-level 0 and debug assertions; release uses opt-level 3 without debug
assertions or LTO. Cargo manifests pin the same profile choices. Nothing is fetched at
build or measurement time. Record `rustc -vV`, host CPU/OS, source base and profile flags
beside the table.

## Artifacts

Run `python3 core/tools/measurement_artifacts.py > /tmp/measurement-artifacts.json`.
This builds or locates the release compiler and the shipping x86-64 runtime archive
with fat LTO, one codegen unit and no debug assertions. Use the returned `compiler`
and `runtime` paths. The ordinary release staticlib has different build settings
and is refused, as is the checked archive. Supplied paths can be checked with
`--compiler PATH --runtime PATH --compiler-profile release`; only `--diagnostic`
permits other artifacts.

Every measurement table row and JSON report carries `compiler_build_profile`,
`compiler_sha256`, `runtime_archive_profile` and `runtime_archive_sha256`. A row
missing any identity, or using other artifacts, is a diagnostic and cannot enter
a result ratio or geomean. Reuse completed Rust samples when correcting the Hirsel
artifacts; preserve the original observations as diagnostics.

## Twins

Each workload has two Rust twins, measured and reported separately; no ratio, total or
geomean combines them.

- **Idiomatic.** The M1 programs' idiomatic twin is `twins/<program>`, which has no crate
  dependencies. A LangArena task's idiomatic twin is the upstream Rust at kostya/LangArena
  3c9f84d, unchanged. Its crates are vendored into the repository (`cargo vendor`) at the
  versions in upstream's Cargo.lock and built with `--offline --locked`: serde 1.0.229,
  serde_json 1.0.154, regex 1.13.1, once_cell 1.21.4, base64-simd 0.8.0, simd-csv 0.14.0,
  rayon 1.12.0 and tokio 1.53.3, with their transitive dependencies as locked. A vendored
  crate's lines are not first-party and never enter the source-line denominator.
- **Matched.** The same algorithm, data representation, container shapes and arithmetic
  checks as the Hirsel program, in safe Rust with no crates: a hand-written matcher where
  Hirsel has no regex, the same JSON representation, checked arithmetic where Hirsel checks.
  A lane other than its author reviews a matched twin before its first ratio is published
  and records the review in the twin's header comment.

Both twins print the Hirsel program's observable output (checksums for LangArena) for every
measured configuration; a twin that does not is not measured.

The serial instruction baseline runs on CPU 16, which is allowed by host rules. Run
one measured process tree at a time, including all compiler workers and linker children.
The pipeline still creates exactly three OS worker threads. They share that CPU for the
serial baseline. This is a baseline of total work, not a multicore-throughput claim.
Later wall-scaling runs use jobs 1/2/4/8 on allowed CPUs with matching limits on both
sides and are separate configurations. Do not add them to a serial instruction ratio.

The script invokes pinned rustc directly, rather than measuring the Cargo shim's
systemd launcher. `check` emits metadata; `build` emits and links the executable.
This keeps every compiler/linker child under the measured process. Cargo builds remain
the normal CI and development path. End-to-end Cargo orchestration is not included in
these rustc numbers and must not be included on only the Hirsel side of a ratio.

## Source-line denominator

`tools/measure.py:source_lines` counts physical Rust lines containing a nonwhitespace,
noncomment token. It lexes line comments, nested block comments, quoted/character/raw
strings and lifetimes. A line containing code and a trailing comment counts once.
Braces and attribute-only lines count. Blank and comment-only lines do not. Count the binary root and its first-party modules once each. Idiomatic roots include
the workload's `program.rs` and shared `support.rs`. Matched roots include
`matched_support.rs` and `matched_storage.rs`, plus LRU/request's
`matched_map.rs`, JSON's `matched_json.rs`, `matched_float.rs` and `matched_sort.rs`,
or pipeline's `matched_sort.rs`. `tools/measure.py:sources` names these source sets. No Cargo manifests,
fixtures, Python, generated code, prebuilt std or other twin's implementation enters
the denominator. Shared driver/trace code is included, so these small compile totals
include a substantial harness/startup floor. Keep source sets and formatting fixed in
comparisons. Count Hirsel physical source with the same comment/token rule and publish
its set; never divide a whole process by just a selected function's lines. Also report
input bytes, source bytes and emitted IR/instance counts when available.

## Sampling and caches

Follow the host's one-sample rule. Take one sample for each workload, layout, profile,
operation and cold/warm configuration. They are distinct configurations, not repetitions
for a median. Do not select the fastest sample or silently retry a measurement.

Cold compilation means a new private output directory with no compiler products or
incremental state. Warm compilation means a second fresh-process compilation of the
same source after the cold compilation, again producing a fresh output with incremental
state disabled. It measures warmed source/std/toolchain pages, not a Cargo no-op. There
is no source edit and no persisted semantic compiler cache in either Rust compilation.
Cold run is the first invocation of the selected executable; warm run invokes the same
executable again with the same input. These programs keep no application cache between
processes. Both include input read, structure construction, output, cleanup and process
startup. Input fixture generation is outside the measurement, equally for both sides.

These cache labels describe private compiler/application state. The shared host's OS
page cache is neither flushed nor claimed cold. Newly linked executable pages may
already be resident. Record this limitation; do not call these isolated-machine cold
page-cache measurements. CPU contention, frequency changes and scheduler noise affect
wall time. There are no confidence intervals from a single sample. Tiny-program ratios
include process startup; report absolute totals and avoid a speed claim from that floor.

## Counters and memory

Each sample runs `tools/capped` with a 2 GiB process-tree memory cap, no swap and no
wall/CPU timeout. Inside that cap, `perf stat -e instructions:u,duration_time` inherits
counters across threads, fork and exec, including linker children. Never put perf
outside a launcher that reparents its compiler children. No system-wide counters are
used. Instructions include the `/usr/bin/time` measurement child and process startup;
keep the same accounting on the comparison side. Duration is perf's nanosecond elapsed
time, converted to seconds. `wrapper_seconds` separately includes capped admission and
scope setup and is never the runtime numerator.

`max_rss_kib` comes from GNU time, the largest process RSS high-water mark in the waited
process tree. It is not a sum of concurrently live RSS. `tree_peak_mib` is capped's
cgroup high-water memory, rounded to MiB; it includes anonymous memory, charged file
cache and measurement tools. Report both and do not label the cgroup number RSS. If
capped uses its process-limit fallback, the tree peak stays missing; do not fabricate
one. Logical charges, allocated bytes and retained slack are different metrics; future
runtime lanes report their curves separately from these physical-memory baselines.

## Reproduction and reporting

```sh
python3 core/tools/measure.py --out /workspace/code/hirsel-briefs/core-corpus-885/measurement-new
```

The evidence directory must be new and outside the repo. The script writes the table
incrementally to `core/BASELINE.tsv` and records the environment. Every normal run is
compared against committed EXPECTED stdout. Raw perf/time/output logs stay in the lane's
evidence directory; only the requested baseline table and environment enter Git.
`BASELINE.md` is a compact view; the TSV contains all 216 configurations.

A failed command, missing counter, wrong output, memory kill, absent build or externally
interrupted run is a row with its status and unavailable fields empty. An externally
timed-out run is recorded as `timeout` by the operator, with its observed bound and
without a ratio. The script adds no timeout. Resume only the missing configurations,
using a new evidence directory and preserving earlier rows. Never rerun a green sample
for confidence. A ratio requires both rows to be complete, correct and equivalent;
a censored result supplies a bound where one exists, never a complete ratio. Cost misses
and noisy/censored measurements are findings; they do not change semantic gates.

Prior art: the [Benchmarks Game method](https://benchmarksgame-team.pages.debian.net/benchmarksgame/how-programs-are-measured.html)
supplies process-tree lifecycle and output checking; [rustc-perf](https://github.com/rust-lang/rustc-perf/blob/master/collector/README.md)
supplies check/build/profile/scenario separation and instructions as the primary metric.
Goose `bench/results.md:25-35,1043-1056` supplies lifecycle/checksum and startup caveats.
The host rule replaces their repeated/best-of sampling with one sample per configuration,
and forbids global cache flushing on this shared host.
