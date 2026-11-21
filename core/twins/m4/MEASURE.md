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
- The raw samples, strace tables, hashes and `measure.sh` are in `w6/` in the std-seed-api-889 evidence archive.

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

## Store rows: which twin is each row's baseline (STORE-SPEC v2.13 §6, Q3)

A row's baseline does the work the spec requires of the store on that row. A twin that skips required work is published beside the fair twin as a bound, never as the target.

| Row | Twin | Status |
|---|---|---|
| R1 target | `store_pcopy.rs` (`m4-store-pcopy R1 <gate>`) | fair: the work listed below |
| R1 reference | `store_im.rs` (`im::OrdMap`, base kept, 64-wide nodes) | reference: wider nodes than the spec's, and no catalog, clock or isolation work |
| R1 bound | `store_mem.rs R1` (`BTreeMap` updated in place, no old version) | bound, never the target |
| R2 | `store_mem.rs R2` (`BTreeMap::range`, a decode per row) | fair: a read keeps no old version, and the store's one Span and one I3 check are per range, not per row |
| R3 | `store_mem.rs R3` (`Arc<BTreeMap>` on a spawned thread) | fair: one lookup of the table's catalog key in the snapshot's catalog map per `get` (I3) |
| R1 indexed overwrite (ticket 5052) | `store_pcopy.rs` (`m4-store-pcopy 0\|1\|3 <gate>`) | fair: R1's put work plus the replaced row's decode and each index's key compare (J1) |
| R4 | `store_journal.rs` (`m4-store-journal R4 <gate> <dir>`) | fair: the work listed below; `durable_commit.rs` (56-byte CRC-32 frame) is a bound |
| R5 | `store_journal.rs` (`m4-store-journal R5 <gate> <dir>`) | fair: the work listed below; `group_commit.rs` is a bound |
| R6 | none | waits for S2; requirement below |
| R7 | none (`open.rs`) | waits for S2; requirement below |
| R8 | none | waits for S2 (the pin waits for S4); requirement below |

**`store_pcopy.rs`, R1's target.** The tree, the commit and the put live in `store_tree.rs`, which `store_pcopy.rs` and `store_journal.rs` share through `#[path]`.
- Node layout:
  - A leaf is a `Vec<Arc<Entry>>` of 16 to 32 rows in key order. A row is one `Arc<Entry>`, and an `Entry` owns its key and value bytes. This matches std.omap's one canonical entry per row.
  - A branch is a `Vec<Child>` of 16 to 32 children. A child is its subtree's minimum row (the same `Arc` the leaf holds) and an `Arc<Node>`.
  - Only the root may hold fewer. A node that reaches 33 splits into 16 and 17, as std.omap does. Ascending puts therefore give both trees the same depth and the same path copied per put.
- What it shares:
  - A put copies only the root-to-leaf path. Each copy is a new vector whose other elements are one count increment each.
  - Every other node, every row and the catalog tree are shared with the base version.
  - The transaction keeps the base roots until publish, so the copy is never skipped. Publish drops them.
- Work per commit (§3.1):
  - one wall-clock read;
  - the body under `catch_unwind`;
  - the resident check;
  - publish at version + 1;
  - no COMMIT encode (a memory store, Q2), but the COMMIT's size is kept by arithmetic per op and checked against `Limit(Transaction)`;
  - a fresh node id per copied node; the key and value limits; I3's write check compares the table's id and schema epoch.
- Work per put:
  - I3's catalog check;
  - encode;
  - the path copy;
  - the op-list append;
  - I4's account update, using the replaced stored entry's size returned by the insert (v2.10 T2).
- Work per fetch:
  - I3's catalog check;
  - the lookup;
  - the `Point` record of the data-tree key;
  - the decode.
- Checks after the gate:
  - fan-out on every node, and key order;
  - 100,000 rows at version 100,000;
  - the account at 110 B per row (§1.11; a 30-byte value).
- Build it as `store_mem.rs` is built (std only, the S4 flags).

**What each twin must do once its slice is on main.**
- **R4** and **R5** (S3), done in `store_journal.rs`:
  - R1's per-commit work through `store_tree.rs`, then the store's exact COMMIT frame (§2.4): 93 bytes for one put (a 30-byte value: main's codec writes String counts as u32 since 07c3aa57), its xxh3-64 check over the 33-byte binding (identity, role 2, segment seq, offset), head[0..8] and payload. At start the frame code is checked against store-build-1010's libxxhash vectors.
  - The resident check counts the frames sent and not yet acked. Then `send` on a channel of capacity 2 × `group_commits` (128).
  - A journal thread per §3.3: a group of up to 64 frames or 4 MiB, one write, one `fdatasync`, the ack, then an END (the END file rewritten whole with truncation, 96 bytes, `fdatasync`) when the queue was idle, after every 64th group, on rotate and on close.
  - Segment creation per §3.2 (HEADER, sync, END at 72, sync, `sync_dir`), and rotation once a segment passes `checkpoint_bytes` (64 MiB). The checkpoint the store starts at that point (pack, manifest) is R6's work and is not in the twin.
  - Build it as `store_pcopy.rs` is built, with xxhash-rust 0.8.15 (`feature="xxh3"`) as its one crate (`--extern xxhash_rust=`), as `core/tools/store_crc_bench.py` builds it. Cargo does not build the twins.
  - After the gate: close, print `IO writes syncs ends groups frames rotates`, then replay every segment (HEADER bytes, each check, the version chain, length = 72 + 93 × frames, the END's d = that length) and compare the rows with the tree's.
- Workload (store-build-1010, 2026-11-16; Hirsel's bench must match it): one table "accounts" in a fresh empty directory on disk, not tmpfs. Commit c (from 0) puts Account {id c % 16, balance c, owner "owner000"} at key c % 16 and returns c. Before READY, 64 commits, each made durable.
  - **R4**: 10,000 commits, each `run` then `durable`. `DONE 50635000 <ns>`. Per commit the queue is idle, so each commit costs one group's write and sync plus the idle END's write and sync.
  - **R5**: commits back to back for 10 s, `durable` after every 64th, the clock read after each `durable`. `DONE <commits> <ns>`; the row compares commits per second. At about 140k commits/s the twin rotates twice in 10 s.
  - STORE-SPEC's R4 row says "16 tables"; the twin and 1010's workload use 16 keys of one table.
- **R6** (S2):
  - Walk the cut's new nodes and encode each NODE frame with its xxh3-64 check.
  - Update a persistent node table's counts.
  - Write and sync a pack, then make the manifest with §3.2's 7 syncs.
- **R7** (S2):
  - Read the same pack and check every frame's xxh3-64 (§2.8).
  - Rebuild the persistent nodes and build the per-node table.
  - `sync` each live segment (§3.2).
- **R8** (S2; the pin in S4):
  - Hold the rows plus a per-node record table (96 B per node, §1.11).
  - For the pin, hold a persistent map that keeps the old version across 10,000 commits.
