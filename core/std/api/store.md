# Store

Package `store`, module `store`, implements the M4 store in Hirsel over
`std.omap.OMap[Bytes, Bytes]`. The S1 package has memory stores, typed tables and
indexes, isolated transactions, read sets, frozen cursors and snapshots,
CallInfo, followers, resident admission and foreign-handle checks.

`memory(clock, random, options)` draws a 16-byte StoreId once. `create_table`
registers key and value schema dictionaries; `table` opens a handle and resolves
older schemas once into shared Readers. Every row starts with its u16 schema
number. Schema numbers are append-only. Fingerprint equality is followed by byte
equality. A key schema change is `Schema`; an unknown row schema number is
`Stale`. Initial table/index id is 1, schema number is 0 and schema epoch is 1.

`run` and `run_on` execute the callback under `isolate`. A successful body publishes
one set of roots after frame encoding and resident admission. An abort, fault or
refusal publishes nothing. An abort returns `Undone { value, ticket: None }`
in memory (§7); reply retention belongs to the streams slice. `fetch` and `range`
read through a shareable `Shape`; `put` and `delete` use a `Table`. `fetch` returns
a fresh decoded value. `put` and `delete` maintain every registered index atomically, including when an extractor
returns Capacity. A handle missing an index returns `Stale`. Dropping and
recreating a name does not make the previous handle valid.

`look(snapshot, body)` runs a view-Tx query under `isolate`, returning
`Seen { value, version, reads }`, `Failed` or `Refused`. It publishes nothing and
runs on a reader task without a Db. The checker refuses a write or `info(mut tx)`
in that body with OWN-5. CallInfo belongs only to `run` and helpers taking mut Tx.
`changes(a, b)` compares data-tree bytes in ascending physical-key order, skipping
shared nodes by node id. Deletions have value None. Different stores or opens
are Foreign; branches of one open are allowed. Catalog and stream roots do not
contribute changes. Node visits are O(k log n), where k counts keys written
between snapshots, including same-value rewrites. The output uses O(w) storage
for w keys whose values differ, with w <= k.

A range records its requested span when it is created and retains that version's
roots. `Rows` can outlive the transaction and move to another task. `Shape` and
`Index` share Readers and let snapshots be queried on reader tasks. `follow`
returns a Latest root; publication follows the admitted head swap. A refused
snapshot-record allocation leaves the follower behind until a later publication.
Closing the writer leaves readers' last snapshots usable.

The resident account is `62 + r8(key length) + r8(value length)` per live entry,
conservative between checkpoints. An overwrite adds and an ordinary main-branch
delete subtracts. Issued node-id ranges are logged before a map operation, so a
fault or Capacity cannot return issued ids to the allocator.

The format is STORE-SPEC v2.4: a 16-byte frame head and seed-zero XXH3-64, bound to
store identity, file role, sequence and byte offset, then head bytes 0–8 and the
payload. S1 emits COMMIT bytes but has no files or background tasks. Its waiting
APIs have immediate memory paths. Disk recovery and checkpoints belong to S2;
journal durability to S3; pins, forks and migration to S4; streams and scopes to
S5. There is no durability promise for a memory store.

`//:core-store-s1` selects the package's evaluator and native laws, including
Cranelift, LLVM development and LLVM release, with look write/info checker
refusals and 32 seeded follower schedules in each configuration. `core/HOOK-ALLOWLIST` gates
the library's core dependencies. The checksum's independent vectors are in
`core/std/tests/check.hirsel`.
