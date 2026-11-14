| Old native payload | LLVM classic | LLVM shared | Cranelift classic | Cranelift shared |
|---|---:|---:|---:|---:|
|1 KiB|603.001|689.000|948.001|923.000|
|2 KiB|846.000|686.000|1191.000|920.000|
|4 KiB|844.000|684.000|1189.000|918.000|
|8 KiB|849.000|684.000|1194.000|918.000|
|16 KiB|854.000|689.000|1199.000|923.000|

The shared helper crosses the classic path at **2,048 old native payload bytes**.
At 1 KiB LLVM is worse, so that point is excluded even though Cranelift improves.
At 2, 4, 8 and 16 KiB the shared path improves user instructions on both engines.
The one policy constant is `abi_layout::GROW_REALLOC_MIN_PAYLOAD_BYTES = 2048`,
capacity times native stride, prefix excluded. No type or engine has a separate
threshold. Below it the existing inline classic path remains.

The original 1 KiB threshold came from inline realloc measurements. These rows
measure the helper that shipped in 4316fbb2. Same base, same kernel, both engines
Speed, system allocator, CPU16, 2 GiB, one serial sample per configuration and
200,000 calls per sample. Kernel: reserve B bytes, push one byte, reserve B more,
read capacity, drop. The classic scratch changes only both emitters' growth
dispatch to false; it is restored after the control build. The shared build is
unmodified 4316fbb2. The control retains the same logical charge and install/refund order.
Every output is exactly capacity 2B with the expected checksum.

| Old payload | LLVM kernel classic / shared | Cranelift kernel classic / shared | Minor faults LLVM classic / shared | Minor faults Cranelift classic / shared |
|---|---:|---:|---:|---:|
|1 KiB|0.420 / 0.659|0.652 / 0.578|1 / 0|1 / 0|
|2 KiB|0.503 / 0.762|0.764 / 1.228|1 / 1|1 / 1|
|4 KiB|0.582 / 1.969|1.206 / 0.635|2 / 2|2 / 2|
|8 KiB|0.674 / 0.688|0.737 / 0.588|2 / 2|2 / 2|
|16 KiB|0.577 / 0.858|0.912 / 0.617|2 / 2|2 / 2|

Kernel instructions are per cycle. Faults are totals over 200,000 cycles; every
major-fault count is zero. Sub-instruction kernel differences include counter
reads and do not choose the threshold. Raw rows and harness scripts are in
`/workspace/code/hirsel-briefs/core-memory-900/shared-crossover/`.
These are later measurements, not M2 or a replacement for the inline crossover
experiment. The former 64 KiB classic fault anomaly remains observed and
unexplained; it is outside this new five-point series.

The fixture inputs move above the new boundary so PLAIN and GRAPH native reuse,
typed native versus portable stride, and HostMemory inside real realloc remain
covered. Full-new admission, original payers, carrier install before refund,
Capacity boundaries, events and observer laws are unchanged.

## Later application rows

Same base 4316fbb2, old shared threshold 1 KiB against the sole change to 2 KiB.
One serial CPU16 sample per configuration, system allocator, 2 GiB; LLVM release,
application Cranelift dev. Arena root 256 MiB, Json root 64 MiB. Every checksum
is exact and all major-fault counts are zero. Raw `shared.json`, `threshold.json`
and per-sample stdout/perf/time files are beside the crossover rows. These are
later rows, not M2. Rust is unchanged and not resampled for this policy change.

| Workload / engine | User G, 1 KiB / 2 KiB | User change | Kernel G, 1 KiB / 2 KiB | Minor faults, 1 KiB / 2 KiB | RSS KiB, 1 KiB / 2 KiB |
|---|---:|---:|---:|---:|---:|
|Binarytrees::Arena llvm|13.668556 / 13.668575|+0.0001%|11.365769 / 11.080429|1,622,250 / 1,622,253|182,612 / 182,600|
|Binarytrees::Arena cranelift|74.338061 / 74.338084|+0.0000%|11.459107 / 10.749836|1,622,258 / 1,622,257|182,888 / 182,632|
|Json::Generate llvm|6.027124 / 6.027189|+0.0011%|0.020728 / 0.020723|2,379 / 2,382|9,216 / 9,216|
|Json::Generate cranelift|30.225119 / 30.225197|+0.0003%|0.039714 / 0.039561|2,397 / 2,396|9,728 / 9,740|

User instructions change by less than 0.002% in every application row; no
application speedup from this threshold adjustment is claimed. The existing
large-buffer page-reuse benefit remains: Arena has about 1.622 million minor
faults and Json about 2,380. Kernel instruction variation is recorded from the
one sample rather than attributed to the threshold.

Qualification: 104 growth/refusal cases per profile, both profiles on both
engines and both architectures; 16 HostMemory sites per engine/architecture,
exact status 102 and stderr, no cleanup. Fixtures retain local/foreign/forwarded,
wide-aligned, GRAPH and native16/portable12 stride coverage around the new
boundary. Both Memcheck runs cover 30 PLAIN/GRAPH growth pairs, exact final
balances, zero errors and zero live blocks. The policy changes physical strategy
only; the logical full-new admission and original refund sequence are unchanged.
