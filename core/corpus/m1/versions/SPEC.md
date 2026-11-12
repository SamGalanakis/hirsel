# Persistent map with version pins

Build a balanced immutable binary search tree over contiguous integer keys. Keep a separate pin on the initial root. Each update path-copies completed immutable nodes and publishes the new root only after every allocation succeeds. Release the previous current root after publication. Finally read all keys through both roots, release the initial pin, then release current. Print sums and actual live-node counts at each boundary. There is no copy-on-write or mutation through a pin.

## Input and size

1,023 keys and 256 deterministic updates, each touching about ten nodes. Idiomatic nodes are Arc<Node> and pins retain only the root. Matched nodes are reusable arena slots with u32 child links and exact explicit edge counts. Both reclaim nodes at the last holder release. Single-task execution isolates persistence from concurrent publication machinery.

The committed normal input is 2,539 bytes. `cases.json` gives every input path and variant; no input is generated inside the measured process.

## Oracles and failure variants

Python maintains the current value array and computes the union of initial-node identities and nodes on changed search paths. It has no reference-count simulation. error appends an invalid key after the two small updates. Capacity rejects draft.node3, the third new node on the first update path, counted leaf-to-root. Every earlier draft node releases and the current and pinned roots retain their original values. No typed CountLimit exists.

For every case C, `inputs/C.txt` comes from the construction just described and `EXPECTED/C.stdout`, `C.stderr` and `C.exit` come from the corresponding independent Python function in `core/tools/reference.py`. Stderr is empty; exit is 0, 2, 3 or 4 under `core/README.md`. The generator never invokes Rust or Hirsel. The values were fixed before Rust output comparison. Resource names in stdout identify lexical owners, rather than allocator blocks. Named admission failures are logical source sites, never native allocator call numbers. Cleanup does not perform another portable admission.

## Hand check

Keys 0..6 sum to 21. Updating 0 to 10 and 6 to 20 gives 45. The changed positions are 0,1,3,5,6, so the union of pinned and current nodes has 7+5 = 12 nodes. Unpin leaves exactly 7; dropping current leaves 0.

## Spec draft correspondence

See `core/SPEC-COMPARISON.md` for this workload's differences from the spec lane's informative example. Runtime expected values come from this contract and the independent references, never from executing that example.
