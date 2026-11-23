# Store

Package `store`, root module `store` (reached as `store.x`, LANG-7), implements the M4 store in Hirsel over
copy-on-write B+ trees of packed pages (STORE-SPEC v2.44). Each table and index has its own
tree. The package provides memory and Format-3 disk/bucket stores, typed tables and
indexes, isolated transactions, read sets, frozen cursors and snapshots,
CallInfo, followers, resident admission, online scrub, offline read-only opens, and foreign-handle checks.

An installed Hirsel toolchain supplies `store@1`. In `hirsel.toml`, add
`store = { major = 1 }` under `[dependencies]`.

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
`changes(a, b)` compares data-tree bytes in ascending logical key order, skipping
shared pages by stamp. Each change holds an Arc to its stored value. Deletions have value None. Different stores or opens
are Foreign; branches of one open are allowed. Catalog and stream roots do not
contribute changes. Page visits are O(k log n), where k counts keys written
between snapshots, including same-value rewrites. The output uses O(w) storage
for w keys whose values differ, with w <= k.

Reads are `Point(Bytes)` or `Span(Bytes, Bytes)` over logical keys,
including the table or index id prefix, with half-open span bounds.
`range` and `range_reverse` return `RangeRows` scoped to the Tx. Each
`next_range` records the gap it inspected, including the row it finds, before
decoding the row. A probe that returns None records the rest of the requested
interval. Constructing a cursor records nothing.
`lookup` returns `IndexRows` scoped to its Tx. It records an index Span when
created and a primary-key Point for each row returned by `next_indexed`.
The checker refuses an escaping IndexRows or a write through its Tx while it
lives; bodies collect results before writing. An unchanged encoded index key
causes no index write. Snapshot `scan`, `scan_reverse` and `find` return
sendable `Rows` advanced by `next`. `count(snapshot, shape, lower, upper)`
returns the number of table rows in that interval in O(log N) tree work. `Shape` and
`Index` share Readers and let snapshots be queried on reader tasks. `follow`
returns a Latest root; publication follows the admitted head swap. A refused
snapshot-record allocation leaves the follower behind until a later publication.
Closing the writer leaves readers' last snapshots usable.

**Keyset pages (STORE-SPEC v2.25 AI1).**
`scan_page(s, shape, lo, hi, limits)` and `find_page(s, index, lo, hi, limits)`
read one bounded page from the same byte cursor as `scan` and `find`.
`Limits{rows, byte_limit}` refuses zero with `Missing("rows")` or `Missing("bytes")`.
A row counts its data-tree key plus its stored value, and a page holds at least
one row when one remains. `Page{rows, byte_count, next}` has `next` = None exactly
on the last page. `Resume` is `send`, not `codec`. It binds the Snapshot's
identity, the table or index id, the upper bound and the last row's full
logical key. `scan_next` and `find_next` resume strictly after that key, so
index entries with equal keys are neither repeated nor skipped. A different
Snapshot gives `Moved(version)`, and another incarnation gives `Stale(id)`. The
law is L36.

The logical account charges each distinct row version
`62 + r8(key length) + r8(value length)` and each distinct collection 126
bytes. It is exact on main without pins or other branches. Accounting with
retained roots can be conservative between settles. The writer logs reserved
stamp ranges before editing pages and consumes them even if the body fails.

The transaction limit is 16,777,280 COMMIT payload bytes, excluding the 16-byte
head. Body-time arithmetic enforces it before publishing. The account uses the
stored value's length, including its schema number, when subtracting an old row.

The disk contract follows STORE-SPEC v2.44 §11.
`open(tasks, dir, clock, random, options)` validates the options, locks the
directory, and validates the selected manifest and page graph before removing
unreachable files. It replays the journal and rotates to a new segment.
Zero `group_commits` or `group_bytes` returns `Missing` naming that field
before taking LOCK. Memory ignores these bounds.
The journal queue holds twice `group_commits` messages. Frames use a 16-byte
head and seed-zero XXH3-64 bound to store identity, role, sequence, offset,
head bytes 0–8 and payload. Unknown optional frames are checked and skipped.

A disk `run` publishes and hands its frame to the journal task. `durable` waits
for a synced prefix; a commit survives any crash once `durable` has returned for
its ticket. `write_place` serves a request source on the sole writer.
It waits for the first request, probes for ready requests, and runs each as a
separate main-branch transaction. It delivers results in request order after
the durability wait. Each result keeps its own value and ticket. A journal
failure leaves covered results deliverable and fails uncovered tickets.
The source's delivery callback owns the request and can drop a disconnected
client's reply. Grouping changes I/O only. END records identify synced
boundaries, while recovery stops at a torn last tail. `checkpoint` rotates
before taking its cut. The writer emits fresh and relocated pages and blobs,
then publishes the manifest after the packs are synced and named. An allocation
refusal leaves the selected generation and journal available for a later cut.
Schema changes publish with the cut.
`close` waits for frames, joins the journal task, releases LOCK, and returns
the first stored I/O error. It never requests a cut. Snapshots remain readable.

The journal ack channel holds 3 × `group_commits` + 3 acks (`open` returns
`Full` if it is refused), and every frame send drains it first, so the journal
task's ack send never waits.

`recorded(db, branch, version)` waits on a write barrier without consuming
journal acks, skipping it only when every frame this Db has sent is acked. It
returns the CALL attached to a Replied operation in the live segments.
The last published generation sets the boundary. A covering cut or memory
mode gives None. A journal
failure is retained on the write-notice channel, so repeated calls return the
same `Poisoned` error. CALL carries key, args, stream and revision; the frame
supplies CallInfo and the stream operation supplies position. `revision` is the
32 bytes `code.revision` gave for the entry that ran, or None when the program
has no revisions. It is written as a u32 length of 0 or 32, any other length is
`Corrupt(Malformed)`, and a Some that is not 32 bytes is `Missing("revision")`
before anything commits.

Streams (§1.8, L13, STORE-SPEC v2.24) live in the streams root, keyed by
stream bytes; a value is the position as u64 LE, the 32-byte `RequestDigest`
bound at it, a state byte (1 Admitted, 2 Replied) and the reply.
`request_digest(key, args)` is SHA-256 of `u8 17 ‖ "hirsel.request.v1" ‖ u32
LE len(key) ‖ key ‖ u32 LE len(args) ‖ args`, allocating nothing (L35).
`admit(mut db, stream, p, d)` answers `Admission`: a new stream admits
position 0 only and is otherwise `Rejected(0)`; `latest + 1` commits
`Admitted(p)`, binding `d`, only once `latest` has its reply, and is otherwise
`Rejected(latest)`. At `p == latest` the stored digest is compared first: a
different `d` is `RequestMismatch` and commits nothing. With an equal digest,
replied is `Duplicate(reply)`; unreplied is `Abandoned` with the ticket this
open committed it under, or, when another open admitted it, commits Admitted
again with the bound digest and answers `Abandoned` with the new ticket. Below
latest is `HistoryUnavailable`, and above `latest + 1` is `Rejected(latest)`.
An admit over `resident_limit` is `Full` and binds nothing. `reply(mut tx,
stream, p, r)` is `Missing("reply")` unless the stream is Admitted at `p`,
and keeps the bound digest. `record(mut tx, call)` attaches one CALL to the
transaction's Replied stream operation. It returns `Exists("call")` for a
second CALL and `Missing("reply")` unless the transaction replied on that
stream. A stream operation adds `9 + key + value` bytes, with a value of
`41 + reply`. A CALL adds `17 + key + args + revision + code` bytes, plus 24
for a nonzero settle. A frame with only stream operations has no data section. A body that
returns Error after a reply commits its reply writes and CALL on the base data
and catalog (F2) and answers `Undone { value, ticket: Some(t) }`; past the
admission bound it is `Refused` and keeps nothing.

The store does no authorization (STORE v2.30 AN1): a Tx may read and write
every key of its store. A program decides access itself, from the Caller it is
given. A live query is registered as caller-independent (`runner.query`, body
`fn(Tx, H, take A)`) or caller-dependent (`runner.query_as`, body
`fn(Tx, H, Caller, take A)`; RUNNER-SPEC v2.25 rule 2). A `query_as` body is
given the subscribing session's bound Caller at its first run, every rerun and
every repair, and its Cover is that subscription's alone: no result is shared
between subscriptions, so one Caller's result never reaches another.

Branches and pins (§1.9, §1.12) are named heads of one open. `fork(mut db, name,
from)` makes a branch at a Snapshot of this open. A live-head fork shares the
parent's roots, adds zero to the logical account, and returns a durable ticket.
On disk it syncs a FORK frame before returning the branch (AP17–AP19).
A fork of an older Snapshot or of a deleted branch counts the proposed heads
and pins against `resident_limit` before making a cut (AP15). A row version
previously held only by a reader joins the account if the new head reaches it.
If the account exceeds the limit, `fork` returns `Full(Resident(c))` with the
exact requested bytes and the limit. `branch(db, name)` finds a branch.
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

## Format 3

STORE-SPEC §11 specifies the paged store, scoped views and journaled forks.

- **Pages.** Trees use packed pages (`Arc[Page]`, 4 KiB target). A cut writes
  the canonical format. A published in-memory image may be unsealed, and readers find entries by slot
  (AP2, AP3).
- **Format 3.** A format-2 directory is `Unsupported(format)`, with no migration.
  The new kinds are PAGE, BLOB, FORK 0x0013 and MANIFEST 0x0031. NODE 0x0020 and
  MANIFEST 0x0030 are retired and refused whatever their MUST bit (AP1). The
  MANIFEST's pack and retirement lists are page trees in their own packs, so
  the MANIFEST holds only fixed fields and branch and pin records, and no cut
  is ever refused for MANIFEST size (AP4, AP14).
- **Per-collection roots.** `Roots` is `{collections, streams, catalog}`, where
  `collections` maps a table or index id to a `CollectionRoot` with its root
  `PagePtr`, exact row count and row charge, and the map leaf owns that root
  page. `count` over a whole table is O(1) after the lookup. `drop_table` removes
  the table's and its indexes' map entries in one schema commit (AP7).
- **Keys.** Read sets, `Change`, `Resume`, covers and COMMIT ops carry the
  logical key (`id u32 BE ‖ encoded key`). Pages store only the suffix past the
  id and the page prefix (AP5).
- **Values.** A stored value up to the store's inline threshold is inline, and
  a longer one is a `BlobRef` to an immutable BLOB in a blob pack. A new store's
  threshold is 1024 bytes; an existing store keeps the threshold its MANIFEST
  records (0–1024). A collection-map descriptor is always inline. Writes are limited by
  `VALUE_LIMIT` = 16,711,680 bytes (`Limit(Value)`); readers check only format
  3's fixed decode ceiling, so a value stored under a larger limit stays
  readable (AP4, AP8).
- **Writes.** A Tx stages row and stream edits against an immutable head.
  Its reads see the staged values and deletes. It builds a private tree when a
  cursor needs page-backed rows. The writer checks admission and prepares
  storage before applying under a continuously unique path (AP9, AP10).
  Failing, aborting or refusing the body leaves the head unchanged.
  `migrate` sorts the output, refuses duplicate keys with `Exists`, and builds
  bottom-up. It publishes only if the exact account of its new roots fits
  (AP13).
- **View reads (AP12).** `fetch_bytes(tx, t, k)` and `get_bytes(s, t, k)` return
  `Result[Option[View[Bytes]], Error]` anchored to the Tx or Snapshot. The bytes
  include the stored schema number and codec payload. The owned `fetch` and `get`
  decode from the view and transcode older schemas as needed.
  `scan_bytes(s, t, lo, hi)` returns `Result[ViewRows, Error] in s`. `ViewRows`
  is scoped to its Snapshot and cannot be sent to a task. `next_view(mut c)`
  returns an owned logical key or `None` at exhaustion. `value_view(c)` borrows
  the current value as `View[Bytes] in c`. It traps if there is no current row.
  The checker prevents advancing the cursor while the value view lives.
  On `Poisoned` the cursor does not move. `ByteRows` is an owning, sendable cursor.
- **Accounts.** Admission counts each distinct row version (tree, logical key,
  stored value) once across every branch head, pin and in-flight fork, at
  `entry(k, v)`, plus 126 bytes per distinct collection. The figure does not
  depend on page layout or history. `resident_limit` bounds this figure alone.
  Journal frames handed to the worker and not yet acked are bounded separately
  by `FRAME_LIMIT` = 16,777,296 bytes, and a commit that would pass it waits for
  acks instead of failing; the frame being built and the worker's group buffer
  have their own fixed bounds. No single figure bounds the store's process
  memory. A delete-only commit is never refused by admission, on disk or in
  memory; like every write it is still refused at the journal cap (below).
  `accounts(db)` returns `{logical, exact, frames, journal, resident}`, where
  `journal` is the journal bytes no published checkpoint covers; resident page
  memory is reported, not admitted, and can exceed `logical` when roots diverge
  (AP15).
- **Checkpoints.** The writer makes each cut itself and serves no other call
  until it ends. It writes only the pages and blobs no earlier cut wrote, plus
  the moved frames of compaction and the changed pack-list pages, and then
  syncs and renames the MANIFEST. A cut that cannot get memory is refused and
  loses nothing: the old MANIFEST and the journal still recover every committed
  write. `checkpoint`, schema commits, `migrate`, `pin`, `unpin`, a fork from an
  older Snapshot, `adopt`, `delete_branch` and install return
  `Full(Allocation(c))` when their cut cannot get memory (install:
  `FullNew(cause)`), with nothing changed. The MANIFEST a cut writes encodes
  the operation's prospective branch and pin records, and the writer adopts them
  only after the rename is durable (AP16). A pause is
  about 0.1–0.25 s for a 10 GB store with 1% changed (an estimate; AP16 gives
  the bound). The checkpoint task and `checkpoint_task_bytes` are removed.
- **Journal cap.** Until a cut succeeds the journal keeps growing, up to
  `journal_limit` (default 268,435,456 bytes; at least `checkpoint_bytes` +
  `FRAME_LIMIT`, or open is `Full(Journal(c))` before it takes the lock). The
  journal counts accepted COMMIT and FORK bytes. A write whose frame would pass
  the limit is refused at once, never waiting, through each operation's own
  result: `run` and `run_on` return `Refused(Journal(c))`. A committing `admit`
  or a live-head `fork` returns `Full(Journal(c))`. `serve` returns
  `Before(Full(Journal(c)), None)`, and `conclude` returns
  `Before(Full(Journal(c)), Some(p))`, handing its Pending back so a retry after
  a checkpoint writes the original context. Here c is
  `Capacity{requested: journal + frame bytes (saturated), available: journal_limit}`.
  A journal refusal is never `NoRoom`, so it never counts toward a claim's
  resident refusals or settle 2. A `checkpoint` or any other cut-made
  operation is the way out (AP15, AP16).
- **Fork.** A Snapshot whose version and three root stamps match its live
  branch head takes the journal path on disk. `fork` returns after the FORK
  sync, with an already durable journal ticket. Memory stores issue the same
  ticket kind without I/O. The fork leaves the logical account and its exact
  flag unchanged. A FORK can trigger a threshold cut. If that cut is refused
  for allocation or id space, the fork still succeeds. If cut I/O fails, the
  Db is poisoned and recovery restores the durable fork. `recorded` skips
  FORK frames and export recovery applies them in journal order.
  The empty name is allowed on both paths.
  A fork past the id counter is `Limit(Ids)`. A fork or `pin` whose record
  would take the branch and pin records past 16 MiB is `Limit(Manifest)`; the
  pack lists never cause it (AP4).
- **Install.** A release install edits each collection by remove (a dropped id,
  with its empty drop record), replace (a created id; zero rows give an empty
  collection) or keep (no range). A schema-only change to a populated table keeps its rows,
  index ranges and root stamps (§11.1). The preparation metadata records NEW's
  `resident_limit`, `pack_bytes`, `checkpoint_bytes` and `journal_limit`, and
  install checks exactly those before activation; a refusal is
  `FullNew(cause)` with OLD still selected (AP1b).
- **Online scrub (v2.42, §11.17).** `begin_scrub` leases the packs referenced by the
  current generation, and `scrub_step` rereads them from disk under a byte budget,
  verifying frame envelopes and reachable pointers without writing or repairing.
- **Bucket authority (v2.43, §11.18).** `open_bucket` stores packs and `MANIFEST-<gen:020>`
  generations in an S3-compatible bucket via `std.object`, fencing a second writer with
  no-clobber conditional puts while keeping the journal local.
- **Offline read-only open (v2.44, §11.19).** `open_read_only` and `open_bucket_read_only`
  open a Format-3 Directory store or current Bucket mirror without writing, exposing
  `read_only_table`, `read_only_get`, and `read_only_version`.
- **Constants and options.** `FRAME_LIMIT: u64 = 16777296` joins `VALUE_LIMIT`.
  `Options` includes `journal_limit: u64` and `authority: Authority` (`Directory() | Bucket(std.object.Bucket)`).
- **Errors.** `Limit` includes `Ids`, `Manifest`, and `Triggers`. `Error` includes
  `Fenced(u64)` and `Remote(std.object.Error)`. Every capacity refusal carries
  `StoreCapacity = Journal(Capacity) | Resident(Capacity) | Allocation(Capacity)`,
  naming the resource that refused: `Error.Full`, `Outcome.Refused`,
  `Looked.Refused`, `Served.NoRoom` (resident or allocation only) and
  `DeployError.FullNew`. `ServeError` becomes
  `Before(Error, Option[Pending]) | After(Error)`. Core `Capacity` and the
  body's `Stop.OutOfBytes(Capacity)` are unchanged, and every other case is
  unchanged (§11.16).
