# M4 W6: Rust twins for the durable one-key commit and bounds-panic isolation

Sources: `core/twins/m4/durable_commit.rs` (`m4-durable-commit`) and `core/twins/m4/isolate.rs` (`m4-isolate`).

`store_im.rs` is the M4 store's persistent reference column from the R1 cost
study. Its R1 workload holds an `im::OrdMap` base version across each put and
drops it after the read-back. It takes `R1` and a gate FIFO path, and uses the
READY/DONE protocol from `core/tools/store_bench.py`. Build it with im15.1.0's
atomic Arc configuration and the pinned dependencies built by
`core/tools/omap_bench.py`, passing `--extern im=<deps>/libim.rlib` and
`-Ldependency=<deps>`. The R1 result is5,000,750,000. It is a reference, with
64-wide nodes, rather than the fan-out-matched target twin; ratio publication
and the required catalog, clock and isolation work belong to that target.

Method:
- rustc 1.99.0 (b940084d7), the twins' release profile: opt 3, overflow checks on, CGU 1, no LTO.
- CPU 16 (`taskset -c 16`), holding /tmp/hirsel-langarena-measure.lock.
- `perf stat -e instructions:u` over the whole process, 5 samples per point.
- A per-operation figure is the median at N minus the median at 0, divided by N. This removes process start and, for the durable twin, the 64-commit history.
- Peak RSS from GNU time; syscall counts from `strace -c`.
- The raw samples, the strace tables, the hashes and the script (`measure.sh`) are in /workspace/code/hirsel-briefs/std-seed-api-889/w6/.

## Durable one-key commit (M4-PLAN §4 matched workload)

The workload is K=16 keys of 8 bytes and 32-byte values, written to a log that already holds 64 commits.

Each commit does four things:
1. It encodes one frame into a reused buffer: `len u32, seq u64, key, value, crc32 u32`, 56 bytes.
2. It appends the frame with one pwrite.
3. It makes the frame durable with one fdatasync.
4. It publishes the key into a `BTreeMap`.

`m4-durable-commit N check` reopens the log and replays it, checking every frame's CRC and sequence. The replayed map must equal the map computed from the workload alone. The 1064-frame check run passes.

| | Rust twin (per commit) | Old store (STORE-RECORD.md:494, K=16, history 64) |
|---|---:|---:|
| instructions:u | 659 (samples 0.341M at N=0, 1.000M at N=1000) | 75.5k |
| log bytes | 56 | 2,528 |
| pwrite / fdatasync | 1 / 1 (1064 of each for 64+1000 commits) | 1 / 1 |
| wall | 77 µs, almost all of it fdatasync | n/a |
| peak RSS | 2,048 KiB at both N | n/a |

The Rust target that M4-PLAN §4 left open ("set when W6 exists") can now be set against 659 instructions, 56 bytes and one sync per commit.

These figures are a floor, not a like-for-like store:
- The twin has no group commit, no tickets, no snapshots and no checkpoint.
- Its frame carries one key and no schema.

The store library's frame will be larger. The ratio is published with its frame layout.

## Bounds-panic isolation (fault, isolate)

`m4-isolate N` runs N bodies under `catch_unwind` with a silent hook.
- Each body allocates a 4-element Vec, which the unwind drops, then indexes a 1000-element table past its end.
- Every body panics with a bounds failure.
- The program asserts that the failure count equals N.

| | per isolated bounds panic |
|---|---:|
| instructions:u | 34,179 (0.283M at N=0, 342.08M at N=10,000) |
| wall | 2.39 µs |
| peak RSS | 1,792 KiB at N=0; 1,792 to 2,048 KiB at N=10,000 |

951's tx twin isolates 10,000 overflow panics inside 100,000 transactions: 496.0M instructions in total (HOOK-FINDINGS.md:131-134). If its panics cost what these do, 342M of that total is unwinding.

The fault library's ratio is its isolate of the same body (an owned buffer, then an out-of-range index), compared with this figure.
