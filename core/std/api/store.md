# Store

Package `store`, module `store`, implements the M4 store in Hirsel over
`std.omap.OMap[Bytes, Arc[Bytes]]`. Keys remain inline and stored values are shared
when a path is copied. The S1 package has memory stores, typed tables and
indexes, isolated transactions, read sets, frozen cursors and snapshots,
CallInfo, followers, resident admission and foreign-handle checks.

`memory(clock, random, options)` draws a 16-byte StoreId once. A
`TableSpec` is built by `spec(name, key, value)` and `with_index(mut s, name, by,
key)`, which returns `Exists` for a repeated index name. `create_table(mut db, s)`
registers the table's key and value schema dictionaries and every index in one
schema commit; `table` opens a handle and resolves
older schemas once into shared Readers. Every row starts with its u16 schema
number. Schema numbers are append-only. Fingerprint equality is followed by byte
equality. A key schema change is `Schema`; an unknown row schema number is
`Stale`. Initial table/index id is 1, schema number is 0 and schema epoch is 1.

`run` and `run_on` execute the callback under `isolate`. A successful body publishes
one set of roots after resident admission. A memory store encodes no COMMIT;
a store with a journal encodes it before admission. An abort, fault or
refusal publishes nothing. An abort with no reply writes returns `Undone {
value, ticket: None }`; an abort that wrote a reply keeps it (F2, below).
`fetch` and `range`
read through a shareable `Shape`; `put` and `delete` use a `Table`. `fetch` returns
a fresh decoded value. `put` and `delete` maintain every registered index atomically, including when an extractor
returns Capacity. A handle missing an index returns `Stale`. Dropping and
recreating a name does not make the previous handle valid. Every read checks the
handle's catalog key against the roots it reads. An old handle reads an older
snapshot normally and returns `Stale(id)` against a recreated table; the new
handle is stale against the older snapshot. Schema epochs constrain writes,
while ids identify table and index incarnations and are never reused.

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
roots. Reads are `Point(Bytes)` or `Span(Bytes, Bytes)` over physical data-tree
keys, including the table or index id prefix, with half-open span bounds.
`lookup` returns `IndexRows` scoped to its Tx. It records an index Span when
created and a primary-key Point for each row returned by `next_indexed`.
The checker refuses an escaping IndexRows or a write through its Tx while it
lives; bodies collect results before writing. An unchanged encoded index key
causes no index write. `range`, `scan` and `find` return sendable `Rows`, which
can outlive the transaction and move to another task. `Shape` and
`Index` share Readers and let snapshots be queried on reader tasks. `follow`
returns a Latest root; publication follows the admitted head swap. A refused
snapshot-record allocation leaves the follower behind until a later publication.
Closing the writer leaves readers' last snapshots usable.

The resident account is `62 + r8(key length) + r8(value length)` per live entry,
exact after each memory commit on main without pins or other branches. An
overwrite replaces the previous entry's charge and an ordinary main-branch
delete subtracts. Accounting with retained roots is conservative between cuts.
Issued node-id ranges are logged before a map operation, so a
fault or Capacity cannot return issued ids to the allocator.

The transaction limit is 16,777,280 COMMIT payload bytes, excluding the 16-byte
head. Body-time arithmetic enforces it before publishing. The account uses the
stored value's length, including its schema number, when subtracting an old row.

The contract follows STORE-SPEC v2.20. `open(tasks, dir, clock, random, options)`
locks the directory, decodes its manifest and packs, replays journal frames and
rotates to a new segment. Zero `group_commits` or `group_bytes` returns
`Missing` naming that field before taking LOCK; memory ignores these bounds.
The journal queue holds twice `group_commits` messages. Frames use a 16-byte
head and seed-zero XXH3-64 bound to store identity, role, sequence, offset,
head bytes 0–8 and payload. Unknown optional frames are checked and skipped.

A disk `run` publishes and hands its frame to the journal task. `durable` waits
for a synced prefix; grouping changes I/O only. END records identify synced
boundaries, while recovery stops at a torn last tail. `checkpoint` rotates
before taking its cut, writes only new nodes, and publishes the manifest after
packs are synced and named. Capacity refuses a cut without poisoning or ending
the worker; a later cut can retry. Schema changes publish on the cut's ack.
`close` waits for frames and cuts, joins both tasks, releases LOCK, and returns
the first stored I/O error. It never requests a cut. Snapshots remain readable.

The journal ack channel holds 3 × `group_commits` + 3 acks (`open` returns
`Full` if it is refused), and every frame send drains it first, so the journal
task's ack send never waits.

`recorded(db, branch, version)` waits on a write barrier without consuming
journal acks, skipping it only when every frame this Db has sent is acked. It
returns a CALL section from live segments. Compaction is
measured by the latest cut this Db has taken, with the previous cut restored
when a refusal is learned. A covering cut or memory mode gives None. A journal
failure is retained on the write-notice channel, so repeated calls return the
same `Poisoned` error. CALL carries key, args, stream and revision; the frame
supplies CallInfo and the stream operation supplies position. `revision` is the
32 bytes `code.revision` gave for the entry that ran, or None when the program
has no revisions. It is written as a u32 length of 0 or 32, any other length is
`Corrupt(Malformed)`, and a Some that is not 32 bytes is `Missing("revision")`
before anything commits.

Streams (§1.8, L13) live in the streams root, keyed by stream bytes; a value is
the position as u64 LE, a state byte (1 Admitted, 2 Replied) and the reply.
`admit(mut db, stream, p)` answers `Admission`: a new stream admits position 0
only and is otherwise `Rejected(0)`; `latest + 1` commits `Admitted(p)` only
once `latest` has its reply; `p == latest` replied is `Duplicate(reply)`;
unreplied, it is `Abandoned` with the ticket this open committed it under, or,
when another open admitted it, commits Admitted again and answers `Abandoned`
with the new ticket. Anything else is `Rejected(latest)`. An admit over
`resident_limit` is `Full`. `reply(mut tx, stream, p, r)` is `Missing("reply")`
unless the stream is Admitted at `p`. `record(mut tx, call)` adds the CALL
section once (`Exists("call")` after that) and is `Missing("reply")` unless
this Tx replied on the call's stream. Each stream op adds `9 + key + value`
COMMIT bytes and a CALL `17 + key + args + stream + revision`; a frame with
only stream ops has no data head. A body that returns Error after a reply
commits its reply writes and CALL on the base data and catalog (F2) and
answers `Undone { value, ticket: Some(t) }`; past the admission bound it is
`Refused` and keeps nothing. The I4 settle re-encodes every section.

Scopes (§1.4, L14): `KeyScope` names the keys a Tx may touch on one store.
`scope(t, access)` covers a table, `scope_keys(t, lo, hi, access)` a key
range of it; `Access` is `ReadOnly` or `ReadWrite`. `union(a, b)` keeps both
scopes' entries (`Foreign` across stores); a key is writable when any entry
holding it is `ReadWrite`. `narrow(mut tx, within, body)` runs `body` under
`within` and restores the scope after; `within` must have each entry inside
one entry of the current scope with equal or greater access, else
`OutOfScope`, and another store's scope is `Foreign`. A `fetch`, `put` or
`delete` of a key outside the scope, or a write to a ReadOnly key, is
`OutOfScope`, as is a `range` whose span the scope does not cover and a
`lookup` on a table the scope has no entry for. Index rows are in scope when
their primary key is; `next_indexed` is `OutOfScope` at the first row outside.
A run starts with every key of its store in scope.

Branches and pins (§1.9, §1.12) are named heads of one open. `fork(mut db, name,
from)` makes a branch at a Snapshot of this open; `branch(db, name)` finds one.
`adopt(mut db, b)` makes b's head main's head only when b is not main, b's parent
is 0 and main's head is still b's fork point; otherwise, or when b is no longer
live, it is `Diverged(b)`. `delete_branch` removes a branch. `pin(mut db, name,
s)`, `unpin` and `pinned` name retained snapshots. Every branch head and pin is
written by each cut and restored by open and replay. A refusal for resident
space first settles the account (§1.11 I4); a schema commit refuses on the
conservative live figure. CALL frames name their branch.

`migrate(mut db, from, to, f)` (§1.2, L31) rewrites every row of `from` through
`f` into the spec `to` in one schema commit. It drops `from`'s indexes, takes the
table id and then the spec's index ids from the id counter in spec order, and
builds the indexes from the new rows. The rows take one new schema number, the
source entry's `next`; numbers are never reused. A Capacity from `f` or an
extractor, or a build past the ceiling, is `Full`; an oversized key is
`Limit(Key)`; two rows with one new key, or a target name naming another live
table, is `Exists`. Each refusal changes nothing. A handle or pin older than
the migrate is `Stale`.

`//:core-store-s1` selects the package's evaluator and native laws, including
Cranelift, LLVM development and LLVM release, with look write/info checker
refusals, scoped IndexRows escape/write refusals and 32 seeded follower schedules
in each configuration. `core/HOOK-ALLOWLIST` gates
the library's core dependencies. The checksum's independent vectors are in
`core/std/tests/check.hirsel`.

`//:core-store-s2` selects pack, manifest, incremental checkpoint, refusal and
recovery laws. `//:core-store-s3` selects journal/END/CALL laws, each W2 crash
cut, cancellation at observed wait entries, named write/sync failures and
32 seeded grouping/rotation schedules per native configuration.

A cut waits for all previously sent frames to be synced before creating the
successor segment whose HEADER binds their end. An empty journal, consecutive
cuts and Barrier-only traffic require no additional ack. A failed prior sync or
a closed rotation sender poisons the Db and takes no Cut.
