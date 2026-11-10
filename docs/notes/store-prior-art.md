# std@1/store: transactions and compaction against SlateDB

Reference: slatedb/slatedb `a5c4fd22b36c836a89d8c4f27bf0b7b045e62e32` (Apache-2.0).

## Transactions

An earlier `std@1/txn_manager` ported SlateDB's `TransactionManager` for
per-key conflict detection between transactions open at once. Transactions
are now entries that hold the store from their run to their commit
(STORE-2), so none can conflict, and the port, its conflict cases and
`Isolation` were deleted.

## Fencing

SlateDB fences a writer by bumping the epoch in its manifest with
compare-and-swap (`FenceableManifest::init_writer`) and writing a fence WAL.
The store has no fence objects: since place-store-681 it is a writer over an
authority (storage.md STORE-11), and a writer holds the history by its claim.
`authority.claim` ends every earlier claim, and an ended claim's
`authority.advance` answers `Fenced` with nothing appended, which is SlateDB's
guarantee made by the authority instead of by the store's layout. The model
negative controls for it are `harness_objects.rs` (`no_cas`, `any_epoch`,
`trust_listing`).

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
