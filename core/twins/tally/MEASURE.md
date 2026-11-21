# std.tally against Rust atomics and per-thread counters

Sources:
- Hirsel side: `core/twins/tally/tally.hirsel`, which uses `std.tally`.
- Rust twin: `core/twins/tally/tally.rs` (`tally atomic N` and `tally local N`).

Each side runs four tasks (or threads) that each add 1 N times, then prints the folded total. The Hirsel worker adds with `std.tally.update(mut h, fn(mut n: u64) { n += 1 })`.

Method:
- The release compiler (sha256 75dd466a…) and the shipping fat-LTO runtime (sha256 0c8ba6b3…) are pinned by `core/tools/measurement_artifacts.py`, built from main at 41493a1a.
- Hirsel is measured optimised (LLVM `--release`), with Cranelift `--release`, and in development (Cranelift dev).
- Rust uses the S4 flags (rustc 1.99, target-cpu x86-64, overflow checks on, CGU 1) at opt 3 and at opt 0.
- Runs are pinned to CPU 16 while holding /tmp/hirsel-langarena-measure.lock.
- Counts are `perf stat -e instructions:u` over the whole process, with 5 samples per point; wall is the median of 5 unprofiled runs.
- Per add = (median at N=250,000 − median at N=0) / 1,000,000 adds.
- Raw samples, scripts and the worker's disassembly are in `tally/` in the std-seed-api-889 evidence archive.

| per add (wall: whole run at N=250,000) | optimised | Cranelift release | development |
|---|---:|---:|---:|
| Hirsel `std.tally.update` | 1.0 instructions (1.41 ms) | 51.0 (5.46 ms) | 79.0 (6.99 ms) |
| Rust `AtomicU64::fetch_add` (Relaxed) | 1.25 (3.38 ms) | | 72.0 (8.92 ms) |
| Rust per-thread u64, joined | 0.0 (1.50 ms; the loop folds to `part = n`) | | 60.0 (7.63 ms) |

Startup alone (N=0) is 1.3 to 1.7 ms on every row.

## Why LLVM reaches 1.0

The combine on the hot path is the call-site closure, a direct call that LLVM inlines, and the partial is a plain `u64` with no Option tag. LLVM keeps the partial in a register, hoists the overflow check out of the loop (it counts how many increments fit before the u64 limit) and steps four adds per four-instruction iteration; the partial is stored back once. The function stored at `new` (`combine`) runs only in `finish`, once per handle.

The Cranelift rows (51 and 79) were not profiled in this pass.
