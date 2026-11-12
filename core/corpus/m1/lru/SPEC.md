# LRU cache with generation-checked slot reuse

Maintain at most 64 entries in an indexed doubly linked recency chain. P key value prepares a variable payload and installs or replaces it; G key returns a copied payload and moves a hit to the front. An absent key leaves order unchanged. At capacity, evict the tail. Save the first inserted handle; S tries it after reuse and must report StaleHandle. Generation advances on reuse. The output contains hit bytes checksum, eviction count and final most-recent-first order.

## Input and size

20,000 operations, 10,000 puts and 10,000 gets, over 257 keys. Values range in length and fit 128 bytes; keys fit 32 bytes. Idiomatic entries are ordinary Vec entries with separately owned String/Vec payloads. Matched entries form a contiguous slot slab with inline bounded payloads and u32 links. Both use a hash index and O(1) unlink/relink.

The committed normal input is 405,196 bytes. `cases.json` gives every input path and variant; no input is generated inside the measured process.

## Oracles and failure variants

Inputs use a fixed modular key sequence. OrderedDict independently models recency and eviction, without reproducing pool links or generations. error appends S after churn has evicted the first occupant. capacity rejects put 2 payload before replacing any slot or mutating index/order. The previous cache remains inspectable; then the cache releases once. These finite fixtures do not exhaust generation counters; their arithmetic remains checked. There is no reference-count limit variant.

For every case C, `inputs/C.txt` comes from the construction just described and `EXPECTED/C.stdout`, `C.stderr` and `C.exit` come from the corresponding independent Python function in `core/tools/reference.py`. Stderr is empty; exit is 0, 2, 3 or 4 under `core/README.md`. The generator never invokes Rust or Hirsel. The values were fixed before Rust output comparison. Resource names in stdout identify lexical owners, rather than allocator blocks. Named admission failures are logical source sites, never native allocator call numbers. Cleanup does not perform another portable admission.

## Hand check

P a one, P b two, G a, P c three leaves order c,a,b. G absent changes nothing. The one successful get returns one, whose bytes sum to 111+110+101 = 322. No entry has yet been evicted.

## Spec draft correspondence

See `core/SPEC-COMPARISON.md` for this workload's differences from the spec lane's informative example. Runtime expected values come from this contract and the independent references, never from executing that example.
