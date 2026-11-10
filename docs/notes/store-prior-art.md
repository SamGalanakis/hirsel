# std@1/store: transactions and compaction against SlateDB

Reference: slatedb/slatedb `a5c4fd22b36c836a89d8c4f27bf0b7b045e62e32` (Apache-2.0).

## Transactions

An earlier `std@1/txn_manager` ported SlateDB's `TransactionManager` for
per-key conflict detection between transactions open at once. Transactions
are now entries that hold the store from their run to their commit
(STORE-2), so none can conflict, and the port, its conflict cases and
`Isolation` were deleted.

## Fencing (`fence.rs`)

The safety properties match: a new writer leaves an empty fence log as the
newest log object, log ids stay contiguous, the fenced writer's next write
fails `Fenced()` and every later call `Closed()`, a writer that fenced
another is fenced in turn by a third, and a reopen holds every acknowledged
commit and no stale one (`DET-1-std-store-fence-chain-run`, after
`test_fence` and `test_fencer_fenced_after_claiming_epoch`). Two deviations,
both kept:

- The fence log is created before the manifest, and the fence log itself
  carries the new epoch (recovery takes the larger of the manifest and log
  epochs). SlateDB first bumps the epoch in the manifest by compare-and-swap
  (`FenceableManifest::init_writer`), then writes the fence WAL. A `Dir` and
  an S3 object store offer create-if-absent, not compare-and-swap on a mutable
  manifest, so the manifests here are immutable numbered objects.
- When the fence id is already taken, `open` fails with `Fenced()`. SlateDB
  refreshes the manifest and retries at the next WAL id
  (`wal/slatedb/writer_init.rs:107-150`) because its epoch is already
  exclusive. Here the taken id may belong to a concurrent opener at the same
  epoch, so the opener cannot retry safely; its caller can reopen. This costs
  liveness under racing opens, never safety.

## Memory and compaction cost

`bench/store/measure.hir`, built with `hirsel build --release`, loads `n` keys
(16-byte keys, 100-byte values) through `store.transact`, 100 keys per commit,
on a real directory, run under `perf stat -e instructions:u` and
`/usr/bin/time` on CPU 7. Instruction counts repeat to within 0.05%. The table
is the store before this change, which leaves compaction as it was.

| n | load (instr) | full compaction (instr) | compaction after 1 more key (instr) | snapshot bytes | max RSS |
|---|---|---|---|---|---|
| 1,000 | 18.85 M | 3.47 M | 3.35 M | 132,042 | 12.3 MB |
| 10,000 | 188.6 M | 34.2 M | 32.0 M | 1,320,042 | 12.5 MB |
| 100,000 | 1,955.3 M | 372.1 M | 317.9 M | 13,200,042 | 34.3 MB (60.2 MB while compacting) |

- **Memory.** The whole index is resident: about 220 B of RSS per entry for
  116 B of data at 100,000 entries, and compaction encodes the whole snapshot
  in memory, about doubling the peak. Linear, so 1M entries is ~220 MB.
- **Compaction.** Every `compact` encodes and writes every entry: 3.7k
  instructions per entry, and nearly as much after one new key as after
  100,000 (the "after 1 more key" column is all compaction but one commit). That is about 19% of the
  instructions it took to load the entries.
- **This change.** Plain `transact` loads cost +0.035% instructions (old
  1,955.32 M, new 1,956.00 M at 100,000). With one serializable transaction
  held open through the load, so every commit's write set is retained up to
  the limits, they cost +2.5% instructions and +4.3 MB RSS.

**Decision: SST-style incremental compaction is deferred.** Compaction is
explicit and costs O(n) in time and in peak memory, but at the sizes the store
serves (an embedded store for an agent's state, ~1e5 entries) one compaction
is 0.37 G instructions. Incremental compaction only pays once compaction time,
or the resident index, outgrows the commits it serves, and fixing memory needs
more than compaction: reads must come from on-disk blocks (block cache,
filters and merging iterators over sorted runs, SlateDB's engine).
Revisit when a workload passes ~1M entries (~220 MB resident) or compacts
often enough that compaction instructions exceed its commits'. The cheaper
first step is a delta snapshot (SlateDB's L0 SST) that writes only the keys
changed since the last snapshot.
