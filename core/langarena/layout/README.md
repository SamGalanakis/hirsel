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


## Completing the service and document cells

`graph_inputs.hirsel` and `graph_inputs.rs` are generated beside `inputs/graph.json`.
Both languages consume those constants. They fix a ring for exclusive/churn and a
linear counted path for snapshots. Edges are u32; the public pool's opaque Id has
u64 identity, slot index and generation. Every removed handle is probed, free slots
reuse LIFO, all256 snapshots are retained, and each traversal includes the shared suffix.
There is one task/thread; no hash table, load factor or Map order applies.
`graph_matched.rs` extracts the old reviewed intrusive-slot shape. The separate
`graph_idiomatic.rs` uses std Vec<Option<Node>>, a generation Vec and a free-index Vec.
Both are safe Rust with checked arithmetic. Idiomatic review925 is approved; matched review925 M1–M4 has a corrected delta
awaiting recheck. Its ratios stay withheld until that review is recorded in the header. Malformed config controls reject zero nodes, an
unrepresentable u32 node count, and an out-of-range edge, with stale/foreign/cleared IDs.

`service.py` runs only selected missing cells, writing raw JSONL outside the repo:

```
python3 core/langarena/layout/service.py --artifacts ARTIFACTS.json \
  --stage runtime --phase exclusive --phase churn --phase rc --phase arc --out NEW-DIR
python3 core/langarena/layout/service.py --artifacts ARTIFACTS.json \
  --observed-runtime OBSERVED-ARCHIVE --stage observe --phase rc --out NEW-DIR
python3 core/langarena/layout/service.py --artifacts ARTIFACTS.json \
  --evaluator CONTRACT-EVAL --stage laws --module graph --module documents --out NEW-DIR
```

Runtime/observe defaults to LLVM release and Cranelift dev; laws also select the evaluator.
`--case` and `--backend` resume missing selections without repeating green work.
`--stage twins` builds both graph twins at matching opt3/opt0 with one CGU/no LTO,
then samples only `--phase` selections; `--skip-twin-laws` retains completed laws on resume.
The ordinary Rust enum is also the idiomatic comparison for sized/streaming documents,
while packed bytes are their matched comparison. That reuses one ordinary sample per
profile; it does not measure the same comparator three times. JSON uses its separate
matched two-pass and serde_json twins. Dev serde dependencies must also use opt0:
`build_twins.py --serde-core-out-dir` uses the cached pinned build-script output to build
serde_core with exactly the same flags as the other dependencies and application.
A supplied prebuilt `--serde-core` must match the requested profile.

Each Capacity variant is a separate phase: exclusive_capacity, churn_capacity,
rc_capacity, arc_capacity, ordinary_capacity, sized_capacity and streaming_capacity.
They have no Rust speed ratio: Rust's allocator does not expose portable MEM-19 refusal.
Pure laws test exact/one-byte-short budgets, all248 sub-budgets for small Rc/Arc snapshots,
all256 ordinary-document sub-budgets, refund after failed path-copy and failed own-data
construction, and reservation retention until a failed region closes. JSON malformed,
unknown escape and invalid UTF8 admission controls are separate from valid-input speed.
The approved std pool private retirement laws cover MAX generations; no public testing
hook or changed pool representation is introduced.

`--events --phase arc` is a separate recording-sink configuration. A native linker
wrapper uses the public OS-process constructor/run API with an event callback. It asserts
8512 creations,512 shares,9024 releases and8512 last releases. Default no-sink samples
and recording-sink samples never blend; the latter have no Rust event-recording ratio.
`count_boundaries.c` is a native ABI law harness, not a Rust twin or source operation.
It symbolically supplies MAX-1/MAX holder counts to the published runtime functions:
MAX-1 shares to MAX, and sharing MAX must stop with status102/CountOverflow before cleanup.
This tests MEM-16's otherwise unreachable-in-a-small-source-test boundary, separately
from both-engine source Capacity/refund laws. Compile it against the selected shipping
archive with `cc -std=c11 -O2 -I crates/hirsel-core-rt/include ... -lpthread -ldl -lm`.

Observer v2 rows retain the complete report: current charged/retained/slack, independent
peaks and run_start/startup/main/run_end intervals. Per-phase executables identify semantic
phases, and graph lifetime identifies the whole sequence. `peak_root_reserved_bytes` aliases
peak charged. Retained counts requested live native backing, not RSS; slack counts exact
unused region chunk tails, not List spare capacity or an inferred builder-trim tail.
Observed runs supply no instruction/RSS numerator. Successful run_end must return all three
current figures to zero. Historical PEAKS.md rows retain their original identities;
current completion receipts and tables live in the lane's SERVICE.md.


Matched admission delta for review925 M3: stable std Rc/Arc have no fallible constructor.
The graph comparison therefore models its single root funding domain with one stack-free
thread-local Cell, not an allocator shim or a new payload field. Values<T,logical-size>
contains exactly one Vec header; reserve/growth admits rounded logical bytes and returns
Capacity with the uninserted owner on refusal. Growth admits the new extent before freeing
and refunding the old. Counted payload Drop refunds32 only on the last strong release;
construction admits before constructing that counted payload, so refused input tails clean
without refunding an unadmitted node. All graph functions propagate Result. Physical std
allocation exhaustion remains fatal, as physical host exhaustion does in Hirsel. This is
restricted to the benchmark's one thread/root domain; it claims no foreign-payer, task,
recording or arbitrary escaped-bound implementation. Both-profile allocation laws cover
all248 small-snapshot sub-budgets, exact successful boundaries, failed Pool growth returning
its owner, and refund to the initial balance after each bounded run. No opaque barrier or
optimization attribute prevents either compiler from proving successful admissions.
