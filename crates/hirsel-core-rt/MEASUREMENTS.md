# Memory cost samples

## Optional Arc recording gate

Source314d049a plus C boolean-extension repair. Same LLVM lane arc-v1 function/seed, Rust1.99 checked opt3/x86-64/no LTO, CPU16/jobs1/2GiB; one sample/configuration. Allocation, initialization, final release and startup stay outside the per-thread instructions:u window. Each of one million iterations shares/releases a retained Arc, reads its runtime payload and mixes the same checksum. Both paths run on the real cooperative scheduler.

| pair | previous | context-gated | Rust Arc | current/Rust |
|---|---:|---:|---:|---:|
| no recorder |61.001|19.000057|13.000054|1.462x|
| checking/counting recorder |113.000|98.000150|13.000054|7.538x|

No-sink meets1.5x. The gated taken path adds79 instructions/pair over untaken, including both context-first calls, event materialization, the checking/counting callback and call-preserved loop state. This is not an isolated hook-body cost. The callback records exactly1,000,000 shares and1,000,000 nonlast releases; all three samples return10158283133157267006. Events and count overflow behavior are unchanged. Disassembly confirms the hook path reads context.events, then task/run/provider directly, with no TLS lookup. LLVM can hoist its invariant context-word load out of the loop. Both LLVM and Cranelift emit the conditional cold call for init/share/release/unwrap.

CONTEXT_EVENTS=8 is a new frozen word, zero without an event sink, otherwise the task's pinned pointer. Root sets it before execution, child inherits recording and installs its own pointer before publication, and it stays immutable during execution. After inline atomic count/test: if context.events !=0, call hirsel_core_arc_initialized(context), hirsel_core_arc_shared(context,node), hirsel_core_arc_released(context,last) or hirsel_core_arc_unwrapped(context,unique). Release records before final cleanup/refund. These hooks have no status, admission, allocation or count mutation. Existing runtime task/channel dispatch remains unchanged. Native runtime count helpers keep their existing internal event route. The C hooks now require nonzero context.events, as generated code proves.

Evidence `/workspace/code/hirsel-briefs/core-memory-900/arc-dispatch-cost/`: copied LLVM function/twin/caller sources, results.tsv, per-configuration stdout/stderr, source environment and disassembly. The archived previous61/113 rows are the LLVM lane's measured source3337b72a, not a fresh resample. Root/child inheritance, actual zero hook-entry count with no sink, recorded stream and schedule tests execute in the runtime suite. Cranelift's stream exposed missing C bool zero-extension after adding the context argument; declaring those C parameters uext preserves the exact0/1 event values.

## Frozen inline regions and matched own pairs

Own source `448174a3`; region source806ecb3f plus the measurement-only frozen-offset shim, unchanged by the later own edits. Same CPU16/jobs1/2GiB/Rust1.99 checked opt3/no LTO method. One million operations per first-run sample, all checksums correct. The own Rust baseline calls the identical backend malloc(N+8)/free, aligns to8 and zeroes the same N payload bytes at offset8. This controls allocator, native size class, alignment and zeroing. Runtime adds admission, the payer prefix and refund. The ordinary Rust Box rows remain historical API comparisons, not accounting overhead estimates.

| payload | system matched Rust | system proven current | extra / ratio | mimalloc matched Rust | mimalloc proven current | extra / ratio |
|---|---:|---:|---:|---:|---:|---:|
| 16 | 151.434 | 163.434 | 12.000 / 1.079x | 66.571 | 78.571 | 12.000 / 1.180x |
| 64 | 154.434 | 166.434 | 12.001 / 1.078x | 71.211 | 83.211 | 11.999 / 1.169x |
| 256 | 166.434 | 178.434 | 12.001 / 1.072x | 152.572 | 164.573 | 12.001 / 1.079x |
| 4096 | 377.434 | 389.435 | 12.001 / 1.032x | 272.074 | 284.074 | 12.000 / 1.044x |

The proven-current sequence uses the payer that lowering proves remains current between allocation and free; inline_own_free_current skips the header load and payer comparison. This is a specialized compiler fact, not a second accounting path or a runtime validation. It still admits, stores the prefix and refunds. Zero-charge blocks retain/release their structural pin through the ordinary helper. The x86-64 steady loop adds exactly12 instructions/pair; whole-process differences span11.9994..12.0009 because initialization/cleanup/code shape contribute under0.001 per pair. Thus the steady target is met, while raw process averages are reported to three decimals. A redundant tag mask on the already untagged plain/align8 payer was removed.

When lowering cannot prove the payer remains current, generic inline_own_free reads the block prefix and compares its original payer to the current record, using the slow refund only for a foreign/forwarded payer. These matched fallback rows remain18.000 extra instructions/pair, an explicit miss of the12 target. Their full costs are system169.434/172.434/184.433/395.434 and mimalloc84.570/89.210/170.572/290.074 for16/64/256/4096, ratios1.119/1.117/1.108/1.048 and1.270/1.253/1.118/1.066. All complete pair ratios meet1.3. MEM-20/MEM-26 require original-payer refunds/growth; they do not require18 instructions. Six of the measured18 are removed when lowering has the current-payer proof. No intrinsic18-instruction spec floor is claimed, and no allocator effect is credited as admission savings.

| region u64 allocation + close | system instructions/op | mimalloc instructions/op |
|---|---:|---:|
| bumpalo inline | 20.441 | 20.590 |
| emitted frozen-offset shim | 25.508 | 25.643 |
| ratio | 1.248x | 1.245x |

The region kernel directly reads/writes abi_layout.rs offsets: compare payer/current, admit once, add the per-run charge, compare cursor/end and bump, then store. It calls region_alloc only for a payer transition and region_refill only for an exhausted chunk. There is no call on the usual bump path. Both allocate one million u64 values and bulk close. Region chunks retain8,060,928 bytes versus bumpalo8,387,200,0.961x, including chunk slack but excluding fixed region/table and vector metadata. Both inline instruction and retained-byte targets pass. This row supersedes the earlier38.51 generic-entry cost as the emitter fast-path measurement.

Evidence: `/workspace/code/hirsel-briefs/core-memory-900/{own-twelve-score,inline-matched-final}/`, including raw instructions, disassembly, stderr byte counters and environment. Reproduce with measure.py --workloads region alloc16 alloc64 alloc256 alloc4096 --modes rust matched inline current. `inline` on own means the general payer check, `current` means the lowering proof; region `inline` is the frozen-offset shim. These are runtime instruction shims, not end-to-end native source benchmarks.

## Task-local custody integration

Candidate `480f33c6`, same provider harness/toolchain/CPU16/jobs1/2GiB method. One Core sample for each named workload; reuse the provider lane's unchanged Tokio references. All checksums pass.

| workload | provider Core instr/op | current Core instr/op | Tokio instr/op | current / Tokio |
|---|---:|---:|---:|---:|
| spawn + join | 3,424 | 2,137.11 | 2,014 | 1.061x |
| channel round trip | 848 | 910.78 | 2,318 | 0.393x |
| three-stage pipeline | 1,278 | 1,405.76 | 2,748 | 0.512x |

Spawn meets 1.2x. The first task-local pilot measured 2,685.47 instructions. Co-locating Context in Task, eliminating both zero-sized slots and using the no-survivor record return lowers that to 2,137.11. The remaining 123 instructions versus Tokio are a whole-lifecycle difference, not a quantified subtraction per mechanism. Source differences remain three separate heap objects for an empty child (Task with Context, prepaid primary Cell, charged record), stable payer slot management, stack switches and mmap/mprotect/munmap of a guarded stack. Kernel work is excluded by instructions:u; user-space mapping wrappers and setup are included. Tokio co-locates its task/future and needs no separate stack. Channel/pipeline are 7.4%/10.0% above the provider lane's Core samples, but remain below both Tokio targets; no improvement over the old Core rows is claimed.

There is no synchronized Funding implementation in the task path. Local admission/refund and scope entry/exit use the task-local table. Foreign refunds emit REFUND then queue to prepaid atomic mailboxes; task_resume checks one domain pending flag and locks/scans only when pending. A foreign growth lease drains prior events before admission, then records CHARGE on success. A received grant executes with the receiving task's owner ID and hands survivors back afterwards. The single worker owns the complete table exclusively; multiple workers must serialize all tasks sharing that table for foreign growth, and through a received finalizer's full execution including suspension. Original-payer growth after its last Context also temporarily pins/reactivates the retired table and recomputes retirement after growth, keeping new blocks alive without another accounting implementation.

`hirsel_core_arc_shared(node)`, `hirsel_core_arc_released(last)` and `hirsel_core_arc_unwrapped(unique)` are the completion hooks LLVM needed, on main since22ef2b37. This slice adds `hirsel_core_arc_initialized()` and moves these memory ABI declarations into abi.rs/the base header. It has no status, allocation, admission or count mutation. Event codes NEW/SHARE/RELEASE/UNWRAP are frozen in abi_layout.rs and the C header. Rc uses ordinary u64 initialization/counts; Arc initializes aligned atomic u64 to 1 before publication, shares with checked relaxed CAS, releases with release decrement/acquire fence at zero, and tests uniqueness with acquire. The scheduler event turn covers inline count and hook; release publishes before last-release glue/refunds.

Evidence: `/workspace/code/hirsel-briefs/core-memory-900/task-local-compact-cost/`; reference `/workspace/code/hirsel-briefs/core-tasks-provider-907/MEASUREMENTS.md`. Reproduce with measure-tasks.py --models core --workloads spawn roundtrip pipeline. Optional memcheck acceptance builds and Rust tests register alternate stacks via the isolated Valgrind helpers moved to hirsel-stack; normal archives add no registration instructions.

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

Evidence: `/workspace/code/hirsel-briefs/core-memory-900/{bound-cold-entry-score,bound-growth-qualified,bound-attribution}/`. Provider task cost, inline region cost and matched own-pair overhead remain pending.

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

Task-local paged payer slots replace synchronized funding references. Outstanding bytes retain slots; structural pins remain only for handles, union edges and zero-charge blocks. Regions aggregate by payer run and a writer keeps its trim token and previous charge. Remote refunds use prepaid mailboxes, drained before payer execution resumes. The provider must serialize foreign-payer growth and received finalizer grants with exclusive payer execution custody; these memory functions do not implement a scheduler.

Reproduce with `python3 crates/hirsel-core-rt/measure.py --out <new-directory>`. Raw samples, environment and logs are in `/workspace/code/hirsel-briefs/core-memory-900/rewrite-endstate/`; the old baseline remains in `measurement/`. The driver compiles the unchanged charge-policy module directly, excluding unrelated shape-serialization dependencies. Build starts wait for load <=32; every compile and sample uses tools/capped at 2 GiB and one CPU. Final C ABI smoke passes against both allocator archives.
