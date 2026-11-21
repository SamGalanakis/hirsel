# Memory cost samples

## M2 frozen memory rows

Measured commit `e96338cf41d71efd2076a9902068048a37b742c8`. Compiler `2f1f30544428e06b3d89bef6ea21cde83d03db780949f339030307055aad9557`; shipping fat-LTO runtime `95dd816277b936c865ccba458657918917889d6cd86a109f8343c1f03f3c8bb7`. These equal the published M2 identities. Full commit/identities are repeated in every row of [M2-RESULTS.json](M2-RESULTS.json). The compiler itself is release; LLVM native code is release and Cranelift is the existing dev configuration.

| LLVM kernel | Hirsel instructions/op | Reused Rust | H/R |
|---|---:|---:|---:|
|allocate-free 128B|200.001|174|1.149x|
|allocate-free 65536B|447.002|421|1.062x|
|clone 16B|224.000|205|1.093x|

| Peak memory | Engine | Root MiB | RSS KiB, default | Charged B | Retained B | Slack B | End C/R/S |
|---|---|---:|---:|---:|---:|---:|---|
|Calculator::Ast|llvm|64|24744|12478016|20545513|58403|0/0/0|
|Calculator::Ast|cranelift|64|25032|12478016|20545513|58403|0/0/0|
|Calculator::Interpreter|llvm|64|45700|25850016|40629988|58403|0/0/0|
|Calculator::Interpreter|cranelift|64|45976|25850016|40629988|58403|0/0/0|
|Binarytrees::Obj|llvm|128|132864|83894520|117449091|58403|0/0/0|
|Binarytrees::Obj|cranelift|128|132864|83894520|117449091|58403|0/0/0|
|Etc::Words|llvm|64|5656|2802248|3875720|58403|0/0/0|
|Etc::Words|cranelift|64|5672|2802248|3875720|58403|0/0/0|
|lru|llvm|64|2304|786720|786883|0|0/0/0|
|lru|cranelift|64|2560|786720|786883|0|0/0/0|
|json|llvm|64|2048|393504|393667|0|0/0/0|
|json|cranelift|64|2304|393504|393667|0|0/0/0|

One sample per configuration, CPU16, existing shared measurement locks and unchanged harness/input bytes. The pair uses 905's counter loop (200,000 calls); clone16 uses 889's small-input counter with empty-loop subtraction (1,000,000 calls). The hot-loop object builder is built from this same frozen commit. Rust174/421/205 are the unchanged supplied samples, reused, not re-taken. Other memory ratios are left to the owning reviewed Rust task tables; none is estimated here.

RSS comes from the default shipping runtime. Balance figures come from separate diagnostic observed runs of the same inputs. Observed code is never a timing sample. Charge is the MEM-19 root reservation including bound/task/grant/startup reservations. Retained is live native backing requests including prefixes/chunks, excluding allocator arenas/bookkeeping/stacks; slack is unassigned region tails. The three independent peaks are not additive or necessarily simultaneous. Every final balance is zero. The observer archive identity is recorded separately per row.

All 12 default and 12 observed outputs match their expected outputs. Binarytrees Obj uses 128 MiB and preserves the exact 83,894,520 B logical peak; native Fixed32 changes physical requests only. Words has identical observed balances on both engines; the earlier single LLVM+3.4% page-count fluctuation is not a logical-charge increase.

Evidence: `m2/` in the core-memory-900 evidence archive; canonical `artifacts.json`, frozen copies/identities, counter logs, per-side RSS and observed reports. Earlier STEP2 measurements are not used as M2 rows because their artifact identities differ.

## Bounded channel and general-own follow-up

One candidate, source c5d520e7 after the Arc gate landed. Same provider kernels, workload counts, CPU16/jobs1/2GiB/Rust1.99 checked opt3/no LTO; one native sample/configuration, all checksums pass. Callgrind uses100,000 items on each side for diagnostic attribution; native pipeline retains1,000,000.

| instructions/item | original provider | pre-fix task-local | after this attempt |
|---|---:|---:|---:|
| round trip |847.72|910.87|748.75|
| three-stage pipeline |1278.09|1405.88|1081.48|

Callgrind reproduced original/current at847.708/911.721 for roundtrip and1284.786/1412.780 for the shorter pipeline. Added work is exactly64/128 instructions per item in the steady scheduler path: empty Funding::drain42/84, and inlined resume binding/normalization22/44. No mailbox had pending data in either message loop, so actual drain locking/scanning contributed0; payer leases contributed0, and record handover occurred only at task completion. The residual below0.02/item is setup/teardown/native memcpy startup variation, not message-path accounting.

The single attempt replaces empty pending.swap with an inline acquire load, with exchange/locking/drain work only on the pending cold side. Normalization still runs because another task's handover can forward the current record. Immutable guard end is cached when the stack is mapped, removing repeated page-size sysconf queries. Original/current sysconf/getpagesize work is116.006/232.009 instructions/item, common to both; removing it is allowed by the guard/custody contract and supplies most of the win. After Callgrind totals are747.719/1084.778. run_root including new inline resume is94.002/219.002 versus current100.002/231.002; out-of-line drain42/84 and repeated page queries116/232 disappear. Native after costs beat the original figures, so no second attempt was made. Cancellation, scheduling choices, event order and guard extents are unchanged.

The general own shim already compared a masked original payer with current. The proposed raw-word equality proof is sound and cheaper by2 instructions. On exact equality and nonzero charge it updates current available/outstanding directly, before tag removal or union lookup. Tagged words safely fall back. Original outstanding bytes prevent slot reuse across exit/re-entry; current always names a canonical active record, including after forwarded-parent normalization and survivor handover. Equality can therefore never identify a new incarnation of a still-charged original slot. Zero-charge blocks use the structural-pin path. `hirsel_core_refund_in(context,original_payer,bytes)` is the C entry for this sequence; abi_layout.rs and both emitters' sequence comments publish it. Existing generic no-context free/refund symbols remain.

| bytes | system matched | equal | different | mimalloc matched | equal | different |
|---|---:|---:|---:|---:|---:|---:|
| 16 | 151.437 | 167.437 | 227.437 | 66.582 | 82.581 | 142.584 |
| 64 | 154.437 | 170.437 | 230.437 | 71.214 | 87.214 | 147.216 |
| 256 | 166.438 | 182.437 | 242.437 | 152.574 | 168.574 | 228.576 |
| 4096 | 377.437 | 393.437 | 453.437 | 272.077 | 288.077 | 348.077 |

Both backends' extra instructions/pair are16.000 when equal and76.000..76.002 when different, against the identical allocator/size/alignment/zeroing baseline. Different means another canonical same-task payer, with no recorder; actual cross-task event/mailbox work is not folded into that number. The equal case improves the old18, but still misses12 by4. Those four instructions are the dynamic payer-word load/current comparison/branch work that the static unchanged-current proof can omit. No MEM rule mandates16 or76 machine instructions; original-payer lookup remains semantically necessary on mismatch. Per the bounded request, no second optimization attempt followed the miss. Tests extend the existing own sequence with exit/re-entry survivors and the context-first C refund hook, without changing the independent balance oracle.

Evidence `bounded-followup/` in the core-memory-900 evidence archive: provider/current/final Callgrind annotations, native-current/final rows, own-final rows, exact first-party baseline96283fdf, build/source environments. Profile helper uses the same production charge.rs and runtime source, not a second implementation. Raw traces and compiled pilots are removed at lane end; source/samples/annotations remain.

## Optional Arc recording gate

Source314d049a plus C boolean-extension repair. Same LLVM lane arc-v1 function/seed, Rust1.99 checked opt3/x86-64/no LTO, CPU16/jobs1/2GiB; one sample/configuration. Allocation, initialization, final release and startup stay outside the per-thread instructions:u window. Each of one million iterations shares/releases a retained Arc, reads its runtime payload and mixes the same checksum. Both paths run on the real cooperative scheduler.

| pair | previous | context-gated | Rust Arc | current/Rust |
|---|---:|---:|---:|---:|
| no recorder |61.001|19.000057|13.000054|1.462x|
| checking/counting recorder |113.000|98.000150|13.000054|7.538x|

No-sink meets1.5x. The gated taken path adds79 instructions/pair over untaken, including both context-first calls, event materialization, the checking/counting callback and call-preserved loop state. This is not an isolated hook-body cost. The callback records exactly1,000,000 shares and1,000,000 nonlast releases; all three samples return10158283133157267006. Events and count overflow behavior are unchanged. Disassembly confirms the hook path reads context.events, then task/run/provider directly, with no TLS lookup. LLVM can hoist its invariant context-word load out of the loop. Both LLVM and Cranelift emit the conditional cold call for init/share/release/unwrap.

CONTEXT_EVENTS=8 is a new frozen word, zero without an event sink, otherwise the task's pinned pointer. Root sets it before execution, child inherits recording and installs its own pointer before publication, and it stays immutable during execution. After inline atomic count/test: if context.events !=0, call hirsel_core_arc_initialized(context), hirsel_core_arc_shared(context,node), hirsel_core_arc_released(context,last) or hirsel_core_arc_unwrapped(context,unique). Release records before final cleanup/refund. These hooks have no status, admission, allocation or count mutation. Existing runtime task/channel dispatch remains unchanged. Native runtime count helpers keep their existing internal event route. The C hooks now require nonzero context.events, as generated code proves.

Evidence `arc-dispatch-cost/` in the core-memory-900 evidence archive: copied LLVM function/twin/caller sources, results.tsv, per-configuration stdout/stderr, source environment and disassembly. The archived previous61/113 rows are the LLVM lane's measured source3337b72a, not a fresh resample. Root/child inheritance, actual zero hook-entry count with no sink, recorded stream and schedule tests execute in the runtime suite. Cranelift's stream exposed missing C bool zero-extension after adding the context argument; declaring those C parameters uext preserves the exact0/1 event values.

## Frozen inline regions and matched own pairs

Own source `448174a3`; region source806ecb3f plus the measurement-only frozen-offset shim, unchanged by the later own edits. Same CPU16/jobs1/2GiB/Rust1.99 checked opt3/no LTO method. One million operations per first-run sample, all checksums correct. The own Rust baseline calls the identical backend malloc(N+8)/free, aligns to8 and zeroes the same N payload bytes at offset8. This controls allocator, native size class, alignment and zeroing. Runtime adds admission, the payer prefix and refund. The ordinary Rust Box rows remain historical API comparisons, not accounting overhead estimates.

| payload | system matched Rust | system proven current | extra / ratio | mimalloc matched Rust | mimalloc proven current | extra / ratio |
|---|---:|---:|---:|---:|---:|---:|
| 16 | 151.434 | 163.434 | 12.000 / 1.079x | 66.571 | 78.571 | 12.000 / 1.180x |
| 64 | 154.434 | 166.434 | 12.001 / 1.078x | 71.211 | 83.211 | 11.999 / 1.169x |
| 256 | 166.434 | 178.434 | 12.001 / 1.072x | 152.572 | 164.573 | 12.001 / 1.079x |
| 4096 | 377.434 | 389.435 | 12.001 / 1.032x | 272.074 | 284.074 | 12.000 / 1.044x |

The proven-current sequence uses the payer that lowering proves remains current between allocation and free; inline_own_free_current skips the header load and payer comparison. This is a specialized compiler fact, not a second accounting path or a runtime validation. It still admits, stores the prefix and refunds. Zero-charge blocks retain/release their structural pin through the ordinary helper. The x86-64 steady loop adds exactly12 instructions/pair; whole-process differences span11.9994..12.0009 because initialization/cleanup/code shape contribute under0.001 per pair. Thus the steady target is met, while raw process averages are reported to three decimals. A redundant tag mask on the already untagged plain/align8 payer was removed.

When lowering cannot prove the payer remains current, generic inline_own_free reads the block prefix and compares its original payer to the current record, using the slow refund only for a foreign/forwarded payer. These matched fallback rows remain18.000 extra instructions/pair, an explicit miss of the12 target. Their full costs are system169.434/172.434/184.433/395.434 and mimalloc84.570/89.210/170.572/290.074 for16/64/256/4096, ratios1.119/1.117/1.108/1.048 and1.270/1.253/1.118/1.066. All complete pair ratios meet1.3. MEM-20 requires original-payer refunds and MEM-26 charges the current limit for growth; they do not require18 instructions. Six of the measured18 are removed when lowering has the current-payer proof. No intrinsic18-instruction spec floor is claimed, and no allocator effect is credited as admission savings.

| region u64 allocation + close | system instructions/op | mimalloc instructions/op |
|---|---:|---:|
| bumpalo inline | 20.441 | 20.590 |
| emitted frozen-offset shim | 25.508 | 25.643 |
| ratio | 1.248x | 1.245x |

The region kernel directly reads/writes abi_layout.rs offsets: compare payer/current, admit once, add the per-run charge, compare cursor/end and bump, then store. It calls region_alloc only for a payer transition and region_refill only for an exhausted chunk. There is no call on the usual bump path. Both allocate one million u64 values and bulk close. Region chunks retain8,060,928 bytes versus bumpalo8,387,200,0.961x, including chunk slack but excluding fixed region/table and vector metadata. Both inline instruction and retained-byte targets pass. This row supersedes the earlier38.51 generic-entry cost as the emitter fast-path measurement.

Evidence: `{own-twelve-score,inline-matched-final}/` in the core-memory-900 evidence archive, including raw instructions, disassembly, stderr byte counters and environment. Reproduce with measure.py --workloads region alloc16 alloc64 alloc256 alloc4096 --modes rust matched inline current. `inline` on own means the general payer check, `current` means the lowering proof; region `inline` is the frozen-offset shim. These are runtime instruction shims, not end-to-end native source benchmarks.

## Task-local custody integration

Candidate `480f33c6`, same provider harness/toolchain/CPU16/jobs1/2GiB method. One Core sample for each named workload; reuse the provider lane's unchanged Tokio references. All checksums pass.

| workload | provider Core instr/op | current Core instr/op | Tokio instr/op | current / Tokio |
|---|---:|---:|---:|---:|
| spawn + join | 3,424 | 2,137.11 | 2,014 | 1.061x |
| channel round trip | 848 | 910.78 | 2,318 | 0.393x |
| three-stage pipeline | 1,278 | 1,405.76 | 2,748 | 0.512x |

Spawn meets 1.2x. The first task-local pilot measured 2,685.47 instructions. Co-locating Context in Task, eliminating both zero-sized slots and using the no-survivor record return lowers that to 2,137.11. The remaining 123 instructions versus Tokio are a whole-lifecycle difference, not a quantified subtraction per mechanism. Source differences remain three separate heap objects for an empty child (Task with Context, prepaid primary Cell, charged record), stable payer slot management, stack switches and mmap/mprotect/munmap of a guarded stack. Kernel work is excluded by instructions:u; user-space mapping wrappers and setup are included. Tokio co-locates its task/future and needs no separate stack. Channel/pipeline are 7.4%/10.0% above the provider lane's Core samples, but remain below both Tokio targets; no improvement over the old Core rows is claimed.

Local admission, refund and scope entry use the task-local table. Growth admits from the current limit. Foreign refunds and hand-overs emit REFUND and publish to prepaid mailboxes. The owner folds credits at its next event. A received finalizer holds custody of its grant until the hand-over returns the unused reservation and records to the live parent limit.

`hirsel_core_arc_shared(node)`, `hirsel_core_arc_released(last)` and `hirsel_core_arc_unwrapped(unique)` are the completion hooks LLVM needed, on main since22ef2b37. This slice adds `hirsel_core_arc_initialized()` and moves these memory ABI declarations into abi.rs/the base header. It has no status, allocation, admission or count mutation. Event codes NEW/SHARE/RELEASE/UNWRAP are frozen in abi_layout.rs and the C header. Rc uses ordinary u64 initialization/counts; Arc initializes aligned atomic u64 to 1 before publication, shares with checked relaxed CAS, releases with release decrement/acquire fence at zero, and tests uniqueness with acquire. The scheduler event turn covers inline count and hook; release publishes before last-release glue/refunds.

Evidence: `task-local-compact-cost/` in the core-memory-900 evidence archive; reference `MEASUREMENTS.md` from core-tasks-provider-907. Reproduce with measure-tasks.py --models core --workloads spawn roundtrip pipeline. Optional memcheck acceptance builds and Rust tests register alternate stacks via the isolated Valgrind helpers moved to hirsel-stack; normal archives add no registration instructions.

## Allocating bounds and emitted growth

Scope source `d571858c`; growth source `6f939ec3`, unchanged by the later scope edits. Same Rust 1.99.0, CPU16/jobs1/2GiB, checked opt3/no LTO method as below. One first-run sample per configuration, one million iterations, verified checksums. Frozen offsets and C signatures are unchanged.

| workload, instructions/op | system | mimalloc |
|---|---:|---:|
| Rust scope/query/exit | 51.434 | 51.571 |
| query-only control | 10.434 | 10.571 |
| C ABI scope/query/exit | 65.434 | 65.571 |
| C ABI scope with allocating callback | 237.434 | 152.071 |
| identical allocating callback without scope | 182.434 | 97.071 |
| **C ABI enter+exit difference** | **55.001** | **55.000** |
| Rust method enter+exit difference | 43.001 | 43.000 |
| emitted growth push | 15.188 | 15.344 |
| fresh Rust Vec push | 17.438 | 18.341 |
| emitted / fresh Rust | 0.871x | 0.837x |
| Option-wrapped runtime push control | 18.186 | 18.344 |
| Rust allocate/copy/free push control | 20.181 | 20.340 |

The scope difference surrounds the same out-of-line allocating callback on both sides. It includes real C calls, reservation and free-list operations; no statically empty-scope elision. It meets the under-60 target. The old direct-method scope loop measured 391.44 instructions; the matching new direct-method loop is 51.43. Callgrind reproduced the old loop at 391.328 and attributed 66 instructions to slot acquisition/init, 75 to generalized release/pins, 31 to admission, 39 to three Funding lookups, and 180 to inline bookkeeping/loop code. Page/slot address calculations contribute about 65 already included in those rows. Page allocation happens at startup, and no union runs in the warmed no-survivor case. A pointer free list, pin transfer and cold survivor/refill functions remove that common work.

The emitted growth shim carries pointer/length/capacity and calls allocation, copy and free only at growth. The non-growing path compares length, stores and increments with no runtime call. Removing the measurement wrapper saves 3.00 instructions/push. Callgrind attributes 2,999,977 instructions per million pushes to the Option carrier. A diagnostic native perf-counter memcpy interposer records 26 calls/8,405,060 copied bytes/741,053 instructions for runtime versus six startup calls/97 bytes/262 instructions for Rust. Copies explain about 0.741 instructions/push of the original remaining gap; admission occurs only at 19 growth points. This is diagnostic attribution, not another benchmark baseline.

The earlier system Rust Vec result was 14.44. Later isolated functions produce 17.44 under the same flags, so fresh ratios include code-generation differences. Against the earlier 14.44 baseline, emitted 15.19 is 1.052x. The absolute three-instruction wrapper removal and absence of calls on the non-growing path are the supported implementation claims.

Evidence: `{bound-cold-entry-score,bound-growth-qualified,bound-attribution}/` in the core-memory-900 evidence archive. Provider task cost, inline region cost and matched own-pair overhead remain pending.

## Earlier representation rewrite samples

Runtime source: `89d34d5d`. Rust 1.99.0, x86-64, CPU 16, one job, checked opt-level 3, one codegen unit, no LTO, debug or incremental state. Each process runs 1,000,000 pairs or builds 1,000,000 elements. The table divides whole-process retired user instructions by that count, including startup and cleanup. All 36 first-run samples produced the expected checksum; no repetitions or uncertainty estimate. These are isolated memory kernels, not end-to-end application measurements.

| workload | system Rust instr/op | system runtime instr/op | ratio | mimalloc Rust instr/op | mimalloc runtime instr/op | ratio |
|---|---:|---:|---:|---:|---:|---:|
| alloc16 | 301.44 | 173.44 | 0.575x | 139.07 | 88.57 | 0.637x |
| alloc64 | 309.44 | 176.44 | 0.570x | 143.56 | 93.21 | 0.649x |
| alloc256 | 468.44 | 188.44 | 0.402x | 167.82 | 174.57 | 1.040x |
| alloc4096 | 428.44 | 399.44 | 0.932x | 346.07 | 294.07 | 0.850x |
| growth | 14.44 | 18.19 | 1.260x | 15.34 | 18.34 | 1.196x |
| region | 20.44 | 38.51 | 1.884x | 20.59 | 38.64 | 1.877x |
| inline Rc | 20.43 | 17.44 | 0.853x | 20.57 | 17.57 | 0.854x |
| emitted empty bound | 12.43 | 14.44 | 1.161x | 12.56 | 14.57 | 1.160x |

Own pairs meet 1.3x at every size; region instructions meet 2x; inline Rc meets 1.2x; the statically empty bound meets 20 instructions. Growth takes 18.19/18.34 instructions per element, close to the previous runtime 18.20/18.37, but the fresh system Rust result is 14.44, making 1.260x a miss of the previous 1.2x envelope. Mimalloc growth is 1.196x. Representation-matched Rust alloc/copy/free growth is 14.18/14.34 instructions per element, making runtime ratios 1.282x/1.279x. No budget or task rule mandates this gap; driver representation and code generation remain part of these kernels.

The empty-bound kernel is the emitted checked compare/subtract/refund sequence with no materialized table identity, valid when no body operation can refer to that scope. It is not a measurement of the generic C entries. Full task-table enter/query/exit takes **391.44/391.57 instructions**, missing 20 by a wide margin. Table lookup, slot setup, handle/union bookkeeping and depth/context updates remain in that path. The language requires reservation and original-payer hand-over (MEM-19/20/25), but does not require this instruction count. No intrinsic 391-instruction floor is claimed. Emitters currently retain generic calls; inlining and empty-scope elision are the emitter lanes' next integration step.

Region chunks retain **8,060,928 bytes** for 8,000,000 payload bytes versus bumpalo's **8,387,200**, or **0.961x**. Slack is 60,928 bytes, 0.762% of payload. Physical-byte counters exclude the fixed region/limit objects and vector metadata; there is no per-object record. Chunk descriptors scale with chunk count, and payer entries with payer runs. RSS and allocator metadata are separate measurements in the raw samples.

The allocation and Rc kernels execute the inline Rust instruction shims an emitter can produce: own admission, native allocation, tagged payer-prefix store and current-payer refund; checked count increment, decrement/test, and slow cleanup only on last release. Both sides zero the entire own payload and touch the same checksum byte. Native plain storage has an 8-byte prefix; graph storage reserves 48 bytes for constant-stack cleanup, resources 16. Own ratios below 1 reflect different allocator entry/zeroing sequences and shifted allocator size classes. Admission is not free: it adds a compare/branch, available and outstanding updates, a payer store, and refund updates. These samples report the complete absolute pair cost, not a universal Box ratio floor.

System allocation remains the default, using native malloc/free and posix_memalign above alignment 16. The optional mimalloc backend uses native mi_malloc/mi_free and the aligned API only above 16. Mimalloc costs fewer instructions for all four own sizes here; system stays the default without an added allocator library and the optional backend remains available. Rust's baseline uses the upstream MiMalloc wrapper. The measured installed C library is mimalloc 2.1.2+ds-2 through libmimalloc-sys 0.1.49 and mimalloc 0.1.52; the optional Cargo dependency normally builds bundled mimalloc v3, which was not measured. Bumpalo is 3.20.3.

Task-local paged payer slots replace synchronized funding references. Outstanding bytes retain slots; structural pins remain only for handles, union edges and zero-charge blocks. Regions aggregate by payer run and a writer keeps its trim token and previous charge. Remote refunds use prepaid mailboxes, folded at the payer’s next event. Each task pays for its own growth and holds custody of the grant while running a finalizer.

Reproduce with `python3 crates/hirsel-core-rt/measure.py --out <new-directory>`. Raw samples, environment and logs are in `rewrite-endstate/` in the core-memory-900 evidence archive; the old baseline remains in `measurement/`. The driver compiles the unchanged charge-policy module directly, excluding unrelated shape-serialization dependencies. Build starts wait for load <=32; every compile and sample uses tools/capped at 2 GiB and one CPU. Final C ABI smoke passes against both allocator archives.

## Provider operations

Release build (`--target-platforms //tools/buck2:release`, runtime `hirsel-core-rt-archives`), `perf stat -e instructions:u`, median of five on CPU 4, a0e660e5 against 61c29695 (before the four-state provider ABI). The Hirsel programs come from `programs/` in the provider-abi-review-1025 evidence archive, built with the release compiler on each side.

| program | engine | 61c29695 | a0e660e5 | change |
|---|---|---:|---:|---:|
| hello | Cranelift | 264,492 | 264,485 | 0.00% |
| hello | LLVM | 264,407 | 264,413 | 0.00% |
| sleep | Cranelift | 266,663 | 265,902 | -0.29% |
| sleep | LLVM | 266,536 | 265,915 | -0.23% |
| 100,000 `mono_ns` | Cranelift | 47,464,180 | 43,364,098 | -8.6% |
| 100,000 `mono_ns` | LLVM | 46,364,091 | 42,264,054 | -8.8% |
| 10,000 small writes | Cranelift | 5,156,665 | 5,136,703 | -0.39% |
| 10,000 small writes | LLVM | 4,686,556 | 4,666,615 | -0.43% |
| 10,000 small writes, OS provider (C) | either | 3,376,444 | 3,518,110 | +4.2% |
| 10,000 small writes, forwarding wrapper (C) | either | 3,416,445 | 3,758,110 | +10.0% |

The two C rows are `measure/provider-wrap-writes.c`; they run no generated code, so they are the same for both engines. A provider whose `wait` and `deliver` are the OS provider's own runs the operation body with direct calls; every other provider, the wrapper here, calls through pointers and pays the identity check: +6 instructions per write against the pointer-only body (3,698,246), while the OS provider saves 12 (3,638,111 before). That trade was accepted on 2026-11-16 (review 1025 F2). **Line: the wrapper row stays at or under 3,776,900 (+0.5%).** `python3 crates/hirsel-core-rt/measure-provider.py --runtime <archive>` measures both C rows and exits 1 over the line.

### Every provider row can fail (CAP-9, tape-build-1037)

A tape refuses an operation before issue with cause `Tape`, so `wall_ms`, `mono_ns` and `random` gained the fail edge that the waiting rows already had. Same method, against ba2444e9; the program is untaped.

| program | engine | ba2444e9 | with the edge | change |
|---|---|---:|---:|---:|
| hello | Cranelift | 263,753 | 263,755 | 0.00% |
| hello | LLVM | 263,686 | 263,657 | 0.00% |
| 100,000 `mono_ns` | Cranelift | 43,363,408 | 43,763,456 | +0.92% (+4 per read) |
| 100,000 `mono_ns` | LLVM | 42,263,354 | 42,563,324 | +0.71% (+3 per read) |

The edge is a branch on the returned status that is never taken: `movzbq`/`test`/`jne` after the call on Cranelift and `cmpb`/`jne` on LLVM. The runtime adds one compare in `drive`, the refused check that records the `Tape` cause. There is no second call and no reload. The frame position that a refusal reports is counted only by a replaying tape, so an untaped run pays nothing for it.

## Isolate and bound calls

One local entry call: decode the argument, call the entry and encode the result inside a `bytes n` bound and `isolate` (RUNNER-SPEC section 8, p03 mode 2, copied unchanged to `measure/isolate-call`). Instructions per call on the release build, (median at 40,000 calls − median at 20,000) / 20,000, medians of five on CPU 4.

| commit | LLVM | Cranelift |
|---|---:|---:|
| ed653bba, fca80e17, 37771760, 41ed2385 | 1,593 | 3,081.5 |
| 1e866455 (an isolate reservation pins its parent scope) | 1,650 | 3,138.5 |
| c61e7279 to bf7cf64f | 1,650 | 3,136.5 |
| the cell's charge returns at its handover | 1,532 | 3,018.5 |

1e866455 added +57 per call: the parent-scope pin (+13 in `isolate_enter`, the price of the rule that a reservation keeps its parent scope alive) and a refund that ran after the handover, so it went through `settle`'s slow path on a merged record (+101 in `settle`, −41 in `Cell::drop`, −16 in `end_id`). That order only chose the limit that receives the cell's 32 bytes. Now the handover itself returns them: the record is its own root until then and the cell is its only charge, so `end_id` hands them to the live limit with the unused bytes (MEM-20), and no `settle` runs. **Line: LLVM at or under 1,540 and Cranelift at or under 3,034 per call (+0.5%).** `python3 crates/hirsel-core-rt/measure-isolate-call.py --hirsel <hirsel> --runtime <archive>` measures both and exits 1 over a line.
