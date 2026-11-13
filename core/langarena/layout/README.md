# Document batches and versioned graphs

`generate.py` fixes the inputs and independent arithmetic checksums. Document tags cycle
through bool, a borrowed ASCII label of variable length, and a three-element numeric array.
8192 rows are constructed/scanned/discarded 64 times. Both representations copy the same
borrowed input. Ordinary uses an enum with String/Vec fields; compact uses active payloads
and an IndexedSpan offset trailer. The sized path computes the exact encoded length; the
streaming path runs its producer once into a declared maximum and trims the unused tail.
FixedSpan emission uses explicit three-element List scratch on both compact twins.
`inputs/documents.json` is also the parse/print comparison input; `malformed.json` and zero
admission are separate refusal controls. Inputs are deterministic, with no random state.

The graph has four distinct phases: exclusive update of8192 linked array slots 32 times;
generation-checked churn of 4096 nodes 16 times, probing every stale ID; then 256 retained
snapshots of a 64-node path, copying 33 nodes per version, separately with Rc and Arc. Every
snapshot traverses all nodes, including its shared suffix. The lifetime entry runs all four
phases in order and releases each phase before constructing the next. One task, no hash
maps, no hidden thread, full u64 generations, u32 edges. Arc uses the default event mode.
Record-mode instrumentation is not inferred from the default run.

Build both artifacts with `--target-platforms //tools/buck2:release`, using
`//:hirsel-core-driver-bin-hirsel-core` and `//:hirsel-core-rt-archives`. Pass the
shipping view's `x86_64-unknown-linux-gnu/libhirsel_core_rt.a`, not the crate's
staticlib output. Run named phases with:

```
python3 core/langarena/layout/run.py --compiler COMPILER --runtime ARCHIVE \
  --phase churn --phase rc --backend both --out /absolute/evidence/check
```

A missing product, wrong checksum or missing run is a failure, never a ratio. `charges`
checks canonical charge-v1 exact/one-byte-short boundaries; its six-bit expected mask 63
has no physical layout assertion. In particular Option niche changes do not change this
oracle unless the language changes logical charges. Arena failure-prefix contracts are
in `core/std/tests/arena.hirsel`; JSON admission/print-prefix contracts are in its std file.

`rust.rs` is the reviewed safe Rust enum/packed-byte and generational arena/path-copy twin.
`json_matched.rs` mirrors std.json's two passes and compact headers/table. Number validation
runs only in sizing; printing appends byte runs without revalidating published text.
`json_serde.rs` uses serde_json 1.0.154. The input has no object-order/duplicate ambiguity.
The two JSON comparisons are reported separately. `build_twins.py` uses rustc 1.99.0 and
already-local locked sources for itoa/memchr/zmij/serde_json plus Buck's std-feature
serde_core 1.0.229 rlib. It fetches nothing, sets no Cargo options and preserves checked
arithmetic, one CGU, no LTO, x86-64 baseline and the chosen optimization profile.

`measure.py` takes already-qualified binaries, pins CPU 16 and runs one process-tree sample
per side under 2 GiB with the shared perf/time sampler. `--native phase=/path/to/binary`
may repeat. JSON requires the two separate twins; graph/document phases use the safe Rust
shape. Peak RSS and cgroup memory are different fields. `build_measure.py` separately
samples check, fresh build, a retained unchanged record and one edited body, checking the
base checksum and hand-derived+1 edited checksum. Its Rust source selects the reviewed
graph functions only. A refused full check has no check ratio; an unchanged Hirsel record
hit is an absolute startup/work count, not a claim about incremental rustc. Raw products,
profiles and result tables stay outside the repository in the lane evidence directory.

The `charges`, `json_reject` and `json_capacity` phases are correctness controls. The
sampler records their native instructions/RSS without a Rust ratio; the valid-input
twins do not model portable Capacity or the same malformed-input result. Snapshot
budgets include both `(31 + 256 * 33) * 32` node bytes and `256 * 8` holder-list bytes,
so the exact budget is 273376 and the one-byte-short budget is 273375. This uses logical
charge-v1 sizes, independently of native Option niches.

`run.py --measure --compiler-profile release` and `build_measure.py` reuse the
LangArena runner's artifact-profile check and record both artifact hashes. Samples
share its CPU16 lock. `build_measure.py --hirsel-only` replaces invalid Hirsel
samples while preserving valid Rust samples. A release staticlib without the
shipping fat-LTO configuration supplies diagnostic counters only.

`PEAKS.md` combines those retained shipping samples with separate diagnostic
memory observations. Build `//:hirsel-core-rt-observed-archives`, then collect with:

```
python3 core/tools/measurement_artifacts.py > /absolute/evidence/artifacts.json
python3 core/langarena/layout/peak.py --artifacts /absolute/evidence/artifacts.json \
  --observed-runtime OBSERVED/x86_64-unknown-linux-gnu/libhirsel_core_rt.a \
  --twins /absolute/release/twins --out /absolute/evidence/peaks
```

The default runs all 13 LAYOUT entries on LLVM, all arena contracts on both
engines, and the available Rust twins under Massif. `--group layout|arena|rust`
and repeated `--phase` select unfinished parts without repeating completed work.
Hirsel's `peak_root_reserved_bytes` includes startup and unused reservations.
Rust's zero-inaccuracy Massif peak counts requested live heap, with allocator
administration and stacks excluded. The controls and arena laws have no Rust
equivalent. The script checks each unchanged oracle and records no instructions
or RSS, so diagnostic emission cannot replace a shipping performance sample.
