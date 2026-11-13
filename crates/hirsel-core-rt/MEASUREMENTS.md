# Memory cost samples

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
