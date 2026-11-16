# std.tally against Rust atomics and per-thread counters

Sources:
- Hirsel side: `core/twins/tally/tally.hirsel`, which uses `std.tally`.
- Rust twin: `core/twins/tally/tally.rs` (`tally atomic N` and `tally local N`).

Each side runs four tasks (or threads) that each add 1 N times, then prints the folded total.

Method:
- The release compiler and the shipping fat-LTO runtime are pinned by `core/tools/measurement_artifacts.py`.
- Hirsel is measured optimised (LLVM `--release`) and in development (Cranelift dev).
- Rust uses the S4 flags (rustc 1.99, target-cpu x86-64, overflow checks on, CGU 1) at opt 3 and at opt 0.
- Runs are pinned to CPU 16 while holding /tmp/hirsel-langarena-measure.lock.
- Counts are `perf stat -e instructions:u` over the whole process, with 5 samples per point.
- Per add = (median at N=250,000 − median at N=0) / 1,000,000 adds.
- Raw samples and the script are in /workspace/code/hirsel-briefs/std-seed-api-889/tally/.

| instructions per add | optimised | development |
|---|---:|---:|
| Hirsel `std.tally.add` | 18.0 | 142.0 |
| Rust `AtomicU64::fetch_add` (Relaxed) | 1.25 | 72.0 |
| Rust per-thread u64, joined | 0.0 (the loop folds to `part = n`) | 60.0 |

## Why the gap is 18 against the study's 1.0

The study's handle held a bare `u64` and added inline. `std.tally.Handle[T]` holds its `combine: fn(take T, take T) -> T` in the resource, so each add is:
- an indirect call to `combine` (5 instructions in the callee for a checked u64 sum);
- the call itself;
- the Option partial's tag store and value store;
- the loop.

LLVM cannot see which function the field holds, so it cannot inline or hoist it, even in the whole-program release module. The cost is the price of a general combine stored in data. Removing it needs either:
- the engine to specialise a callee that is constant-propagated through a resource field (a core change); or
- a call-site combine (`add(mut h, x, f)`), which changes the API.
