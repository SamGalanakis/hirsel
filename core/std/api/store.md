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
refusal publishes nothing. `fetch` returns a fresh decoded value. `put` and
`delete` maintain every registered index atomically, including when an extractor
returns Capacity. A handle missing an index returns `Stale`. Dropping and
recreating a name does not make the previous handle valid.

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

The format is STORE-SPEC v2.2: a 16-byte frame head and seed-zero XXH3-64, bound to
store identity, file role, sequence and byte offset, then head bytes 0–8 and the
payload. S1 emits COMMIT bytes but has no files or background tasks. Its waiting
APIs have immediate memory paths. Disk recovery and checkpoints belong to S2;
journal durability to S3; pins, forks and migration to S4; streams and scopes to
S5. There is no durability promise for a memory store.

`//:core-store-s1` selects the package's evaluator and native laws, including
32 seeded follower schedules on each native engine. `core/HOOK-ALLOWLIST` gates
the library's core dependencies. The checksum's independent vectors are in
`core/std/tests/check.hirsel`.
