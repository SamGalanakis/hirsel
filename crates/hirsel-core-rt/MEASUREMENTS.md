# Memory cost samples

Runtime source: 62fe0ded, before later Clippy-only conditional spelling and atomic-method renaming. Rust 1.99.0, x86-64, CPU 16, one job, opt-level 3, checked overflow, one codegen unit, no LTO/debug/incremental state. Each process runs 1,000,000 operations or builds 1,000,000 elements. One instruction sample per configuration, including process startup and cleanup; shared OS page cache was not flushed. All 34 samples produced the expected checksum. This is a Rust driver of the runtime, before compiler integration.

| workload | system Rust instructions | system runtime instructions | ratio | mimalloc Rust instructions | mimalloc runtime instructions | ratio |
|---|---:|---:|---:|---:|---:|---:|
| alloc16 | 301,436,130 | 631,444,976 | 2.095x | 138,084,033 | 632,070,615 | 4.577x |
| alloc64 | 309,436,290 | 634,444,698 | 2.050x | 87,602,232 | 637,669,046 | 7.279x |
| alloc256 | 468,435,286 | 644,457,316 | 1.376x | 106,385,441 | 658,298,340 | 6.188x |
| alloc4096 | 459,435,843 | 885,439,864 | 1.927x | 344,089,642 | 803,097,542 | 2.334x |
| growth | 17,442,220 | 18,195,099 | 1.043x | 18,353,033 | 18,366,476 | 1.001x |
| region | 20,444,070 | 477,863,324 | 23.374x | 20,606,492 | 478,097,807 | 23.201x |
| rc | 20,434,982 | 49,436,665 | 2.419x | 20,583,594 | 49,587,330 | 2.409x |
| limits | 12,434,786 | 746,444,320 | 60.029x | 12,583,469 | 746,593,985 | 59.331x |

The 1.2x target passes only growth: 1.043x with the system allocator and 1.001x with mimalloc. Against representation-matched Rust growth, which also allocates/copies/frees instead of reallocating, the ratios are 0.901x and 0.902x. Idiomatic Rust uses Box/Vec/bumpalo/Rc; its empty-limit reference is a checked scalar reservation/refund with an availability checksum. The runtime empty-limit sample also queries availability, so its number includes that extra synchronized read. This comparison measures that empty-scope workload, not a Rust implementation of the complete union-find funding protocol.

System/global allocation remains the default. Its runtime instruction count is lower in seven of eight workloads; mimalloc improves 4096-byte pairs by 9.3%, while increasing grow-workload max RSS from 10,244 to 18,176 KiB. The installed C allocator is mimalloc 2.1.2+ds-2, reached through the unchanged mimalloc 0.1.52/libmimalloc-sys 0.1.49 Rust wrappers. This is not a measurement of the wrappers' bundled mimalloc v3. Bumpalo is 3.20.3. No dependency is downloaded or copied.

The remaining gap is explicit bookkeeping. Every own pair retains/releases a funding record, checks/refunds the logical charge under the forest mutex, and carries an 80-byte native header plus its backpointer. Region allocations additionally retain a payer and charge record each; 8,000,000 logical payload bytes retain 48,037,888 physical chunk bytes versus bumpalo's 8,387,200. Chunk slack alone is 37,888 bytes; the rest is per-allocation bookkeeping. Runtime region max RSS is 48,640/49,152 KiB and cgroup peak 57 MiB, versus 9,728/9,984 KiB RSS and 18/19 MiB peak for bumpalo. Rc adds out-of-line share/release and the constant-stack dispatch. Empty limits exercise mutexes, reference counts, reusable forest slots and scope-stack operations against a scalar Rust counter. No missed target is treated as a pass.

Reproduce with `python3 crates/hirsel-core-rt/measure.py --out <new-directory>`. Raw perf/time/stdout/stderr and environment.json live at `/workspace/code/hirsel-briefs/core-memory-900/measurement/`; the original samples.tsv remains unchanged. Build starts wait for host load <=32. Measurements use tools/capped at 2 GiB and perf instructions:u inside the cap. Only one measured process runs at a time.
