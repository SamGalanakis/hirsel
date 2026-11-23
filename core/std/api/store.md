# Store

Package `store`, root module `store` (reached as `store.x`, LANG-7), implements the M4 store in Hirsel over
copy-on-write B+ trees of packed pages (STORE-SPEC v2.44). Each table and index has its own
tree. The package provides memory and Format-3 disk/bucket stores, typed tables and
indexes, isolated transactions, read sets, frozen cursors and snapshots,
CallInfo, followers, resident admission, online scrub, offline read-only opens, and foreign-handle checks.

An installed Hirsel toolchain supplies `store@1`. In `hirsel.toml`, add
`store = { major = 1 }` under `[dependencies]`.

`memory(clock, random, options)` draws a 16-byte StoreId once.

**One handle per table (DX Q1, STORE-SPEC v2.57 §1.2).** `Table[K, V]` is plain
`shareable + send` data: the StoreId, the table id, its catalog key, `Key[K]` and the
shared value Readers. The same handle reads and writes. There is no `Shape` and no
`store.shape`: a read-only use is a view `Tx` (`tx: Tx`), and OWN-5 refuses a write
through it. An `Index[J, K, V, P]` is shareable as well. Index extractors are not part
of any handle. They live in the open's **index registry**, which is keyed by table id
and owned by the `Db`, or by the `Follower`, which holds no extractors. A write `Tx`
reaches the registry, so `put` and `delete` maintain every index the catalog lists.
A handle never holds a stale subset of the indexes. Every row starts with its u16 schema
number. Schema numbers are append-only. Fingerprint equality is followed by byte
equality. A key schema change is `Schema`; an unknown row schema number is `Stale`.
Table and index ids are never reused. A handle checks its catalog key against the
roots it reads, and an id that is not live there is `Stale(id)`.

**Declaring the schema.** A program declares its tables once, in
`schema(mut d: store.Declare) -> Result[H, store.Error]`. The runner (or a test)
runs it in one of three modes:

- **`Migrate`** (genesis, `--hirsel-install`, a deploy's prepare). It creates
  missing tables and indexes, appends a changed but compatible value schema, and
  rebuilds an index whose kind, key schema, payload schema or extractor/projection
  revision changed. It drops an index of a declared table that the declaration no
  longer names. A table the declaration no longer names makes the whole call
  `Undeclared(name)`, unless the optional `on_migrate(mut m: store.Migration)`, which
  runs first, dropped or renamed it. Every change is in one schema commit, so an
  error leaves the old catalog and rows exactly as they were.
- **`Open`** (an ordinary open, an offline read-only open). It only checks: a
  missing table or index is `Missing(name)`, a different key or payload schema is
  `Schema`, a different extractor or projection revision is `Changed(name)`, and a
  catalog index the declaration does not name is `Undeclared(name)`.
- **`Follower`** (M2 §12.5). It binds the same declaration over a follower's applied
  catalog, with `Open`'s checks. The handles are read-only and carry no extractors.

There is no describe mode. A describe launch runs `Migrate` on an empty memory store
and reads the catalog it built. `declare_table(mut d, name, key, value)` returns a
`TableDecl`. `index`, `covering` and `covering_row` add indexes to it, and
`finish(mut d, take t)` seals it and returns the `Table`. A `TableDecl` that has not
been sealed cannot be read or written, so a table without its indexes cannot exist.
Keys are written with `std.codec.key_of[K]()` ([codec.md](codec.md)), which covers
`Desc[T]` for descending components.

`run` and `run_on` execute the callback under `isolate`. A successful body publishes
one set of roots after resident admission. A memory store encodes no COMMIT;
a store with a journal encodes it before admission. An abort, fault or
refusal publishes nothing. An abort with no reply writes returns `Undone {
value, ticket: None }`; an abort that wrote a reply keeps it (F2, below).
`fetch`, `range`, `range_prefix`, `count`, `lookup`, `lookup_prefix` and `entries`
read through a `Table` or `Index` in any `Tx`; `put` and `delete` need `mut Tx`.
`fetch` returns a fresh decoded value. `put` and `delete` maintain every index in
the registry atomically, including when an extractor returns Capacity, and a
`Conflict` (below) changes nothing. Dropping and recreating a name does not make the
previous handle valid. An old handle reads an older snapshot normally and returns
`Stale(id)` against a recreated table; the new handle is stale against the older
snapshot.

**Indexes (DX Q3).** An extractor is `StaticFn[fn(K, V) -> Result[List[J], Capacity]]`.
An empty list puts the row in no entry (a partial index), one key gives a standard
index, and several give a multi-index. The store sorts the encoded keys and removes
duplicates, all inside the write's bounds. The kind is `Many` (entry
`ix ‖ J ‖ K => payload`) or `Unique` (entry `ix ‖ J => u32 len(K) ‖ K ‖ payload`). The payload is
`()` for `index`, a projection `fn(K, V) -> Result[P, Capacity]` with its own codec for
`covering`, and the row itself for `covering_row`, the identity shorthand. The catalog
stores each index's kind, key schema, payload schema list (append-only numbers, like a
table's) and the `code.identity` and `code.revision` of the extractor and the
projection. So an edited extractor body is seen (`Changed` in `Open`, a rebuild in
`Migrate`). An unchanged encoded key with an unchanged payload causes no index write.
A changed payload at an unchanged key rewrites the entry, so a covering slot sees a
value-only edit.

**Unique conflicts.** Before `put` stages anything, it computes the row's complete
edit: old and new entries of every index, the primary row, and their frame and resident
charges. For each `Unique` index, in declaration order, and each new J in encoded order,
it probes the staged view of `ix ‖ J`. An entry owned by another primary key is
`Conflict(index name)`, never `Exists`. The row's own key is not a conflict, so a row
keeps its unique keys across updates. Nothing is staged, and the `Tx` becomes
**abort-only**: every later `put` or `delete` returns the same `Conflict`, reads still
work, and the run commits no data. A body that returns `Error(e)` keeps only its reply
(F2), so `?=>` maps a conflict to a domain error such as `Taken`. A body that returns
`Ok` from an abort-only `Tx` is a fault: `run` answers `Failed`, and the runner answers
`Failed(Conflict(name))`, so no reply reports success for discarded writes. To swap
two rows' unique keys, delete one and put both. The probe records a `Point` on
`ix ‖ J`, so a query that tested a name for availability re-runs when it is taken.
A `Migrate` backfill checks uniqueness per J as it merges its sorted runs, and a
duplicate is `Conflict(name)` with the old catalog and rows unchanged.

`look(snapshot, body)` runs a view-Tx query under `isolate`, returning
`Seen { value, version, reads }`, `Failed` or `Refused`. It publishes nothing and
runs on a reader task without a Db. The checker refuses a write or `info(mut tx)`
in that body with OWN-5. CallInfo belongs only to `run` and helpers taking mut Tx.
`changes(a, b)` compares data-tree bytes in ascending logical key order, skipping
shared pages by stamp. Each change holds an Arc to its stored value. Deletions have value None. Different stores or opens
are Foreign; branches of one open are allowed. Catalog and stream roots do not
contribute changes. Page visits are O(k log n), where k counts keys written
between snapshots, including same-value rewrites. The output uses O(w) storage
for w keys whose values differ, with w <= k. `changes_page(r, a, b, lo, hi, limits)`
returns the same changes restricted to `[lo, hi)`, one bounded page at a time, with
the resume key of the next page.

Reads are `Point(Bytes)` or `Span(Bytes, Bytes)` over logical keys,
including the table or index id prefix, with half-open span bounds.
`range` and `range_reverse` return `RangeRows` scoped to the Tx. Each
`next_range` records the gap it inspected, including the row it finds, before
decoding the row. A probe that returns None records the rest of the requested
interval. Constructing a cursor records nothing. `range_prefix(tx, t, q, key_of[Q]())`
and `lookup_prefix(tx, ix, q, key_of[Q]())` read every key whose leading components
equal `q`. Q's key schema must be a component prefix of the key's, or the call is
`Schema(0)`. The span is `[enc(q), successor(enc(q)))`, where `successor` drops trailing
`0xff` bytes and increments the last byte; a prefix of all `0xff` is bounded by the end
of the id's range. `count(tx, t, lo, hi)` records one Span and costs O(log N).
`lookup` returns `IndexRows` scoped to its Tx and joins each entry to its primary row.
Each `next_indexed` records the index gap it scanned in full (J, K) order, ties
included, and a primary-key Point for the row it returns. Exhaustion records the
terminal gap. So a write past the last row a top-k query took does not invalidate it.
`entries(tx, ix, lo, hi)` returns the index's own `(J, K, P)` without the join and
records only index gaps. The checker refuses an escaping IndexRows or a write through
its Tx while it lives; bodies collect results before writing. Snapshot reads use M3's
per-task `Reader` (STORE-SPEC M3, M3 Q2): `get(mut r, s, t, k)`, `scan`, `find`,
`count`, `scan_page` and `find_page` all take `mut r: Reader` and `waits`, and return
owned values. `follow` returns a Latest root; publication follows the admitted head
swap. A refused snapshot-record allocation leaves the follower behind until a later
publication. Closing the writer leaves readers' last snapshots usable.

**Keyset pages (STORE-SPEC v2.25 AI1).**
`scan_page(mut r, s, t, lo, hi, limits)` and `find_page(mut r, s, index, lo, hi, limits)`
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
its ticket. Grouping changes I/O only. END records identify synced
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
`fn(Tx, H, Caller, take A)`; RUNNER-SPEC v2.45 rule 2). The publisher evaluates one
**shared view** per `(entry, argument bytes, None)` for `query` and per
`(entry, argument bytes, Some(the Caller's full codec bytes))` for `query_as`. So
subscriptions share a result only when the body ran on the same inputs. A
`query_as` result reaches only sessions bound to that exact Caller, and one Caller's
result never reaches another (RUNNER-SPEC v2.45 §4 "Shared views").

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

`migrate_table(mut m, from, to, f)` (§1.2, L31) is a `Migration` operation, so it
runs only in `on_migrate`, before `Migrate` mode's declaration. It rewrites every row of
`from` through `f: fn(K, V) -> Result[Option[(K2, V2)], Capacity]` into the table `to`
names, and the declaration that follows builds that table's indexes from the new rows.
The rows take one new schema number, the source entry's `next`; numbers are never
reused. A Capacity from `f` or an extractor, or a build past the ceiling, is `Full`;
an oversized key is `Limit(Key)`; two rows with one new key, or a target name naming
another live table, is `Exists`; a unique duplicate is `Conflict`. Each refusal changes
nothing. A handle or pin older than the migrate is `Stale`. `drop_table(mut m, name)`
and `rename_table(mut m, from, to)` are the other `Migration` operations; they are how
a table leaves the declaration.

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
- **View reads (AP12, M3 Q2).** `fetch_bytes(tx, t, k)` returns
  `Result[Option[View[Bytes]], Error] in tx`, a zero-copy view anchored to the Tx and
  usable inside `run` and `look`. The bytes are the stored schema number and the codec
  payload. The owned `fetch` decodes from the view and transcodes older schemas as needed.
  There are no bare-Snapshot view reads: `get_bytes`, `scan_bytes`, `ViewRows` and
  `ByteRows` are deleted. A Snapshot read outside a `look` goes through a `Reader` and
  returns owned values.
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
  `read_only_table`, `read_only_get`, and `read_only_snapshot` (the captured `Snapshot`).
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

## Sizing a store (DX Q6a)

`store.defaults()` and its fixed 1 GiB `resident_limit` are deleted. `sized(kind, share)`
derives every byte figure from `share`, the bytes this store may use, and returns
`Result[Options, Shortage]`. `kind` is `Memory | Directory | Bucket(std.object.LaneConfig)`. The fractions are
STORE-SPEC v2.60 §1.1 "Derived sizes". The runner computes `share` from the
executable's root limit (`hirsel build --limit`), minus `operate_bytes` and main's keep,
so a place has no store numbers to choose. `Overrides` holds an `Option` per figure,
and each `Some` replaces the derived value. The result must still fit `share`. A
`Shortage { need, have, parts }` names every part of the requirement with its bytes,
so the diagnostic shows which figure to raise. There are no named profiles.
A store with a Tiered collection carves its cache, writer pins, dirty bytes and pager
state out of `resident_limit` by one equation (STORE-SPEC v2.60 §1.1 "The Tiered
plan"); a store without one reserves none of them.

## Projectors: durable derived state from `changes`

A **projector** keeps derived state, such as a search index, embeddings or a
cross-place projection, current with a source table. It reads source changes through
`changes_page` and writes derived rows and its cursor row in **one** transaction of
the derived store. Readers see one complete **image** at a time: never a half-applied
pass (STORE-SPEC v2.60 §1.14).

```hirsel
pub type Binding: codec { store: StoreId, branch: u32, table: u32 }
pub type Image: codec { generation: u32, position: u64, slot: u8 }
pub type Pass: codec { generation: u32, target: u64, resume: Option[Bytes] }
pub type Projection: codec { binding: Option[Binding], image: Option[Image], pass: Option[Pass],
        retiring: Option[u32], next: u32, again: bool }
pub type Source = Local | Follower(Follower)
pub opaque type ProjectionDecl: shareable + inert + send
pub opaque type Projector[K, V]          // not send
pub type Delta[K, V] { generation: u32, at: u64, key: K, value: Option[V] }
pub type Fold[E: inert, K, V, A] { env: E, apply: fn(mut Tx, E, Delta[K, V]) -> Result[(), A], drop: fn(mut Tx, E, u32, Limits) -> Result[bool, A] }
pub type Stepped = Updated(u64) | Paged(u64) | Published(u64) | Dropped(u32) | Reloaded | Idle
pub type ProjectError[A] = Store(Error) | Apply(A)
pub fn projection(mut d: Declare, name: String) -> Result[ProjectionDecl, Error]
pub fn projector[K, V](decl: ProjectionDecl, source: Table[K, V], from: Source, r: Reader) -> Projector[K, V]
pub fn step[K, V, E: inert, A](mut p: Projector[K, V], mut db: Db, limits: Limits, fold: Fold[E, K, V, A]) -> Result[Stepped, ProjectError[A]] waits
pub fn rebuild[K, V](mut p: Projector[K, V], mut db: Db) -> Result[Ticket, Error] waits
pub fn binding[K, V](source: Table[K, V], b: Branch) -> Binding
pub fn live(row: Projection, b: Binding) -> Option[u32]
```

- **Declare, then make the handle.** `projection` declares the reserved table
  `hirsel.projections` in the derived store's `Declare` and returns a shareable, inert
  declaration that can live in a place's H. `projector` makes the task-local handle from
  it, the source Table (in `db` for `Local`, read through an M2 follower for `Follower`)
  and a Reader.
- **The row binds its source.** The row records the source's StoreId, branch and table
  id. A handle on another source retires the image and rebuilds, and `live` returns None
  until then. Each commit re-reads the row and writes nothing if it changed since the
  step loaded it (`Reloaded`), so two handles never overwrite each other.
- **One published image.** `live(row, binding(source, b))` names the image's generation,
  or None before the first publish or when the row is bound to another source. A reader
  keeps the declaration and the source Table in its inert env and reads the row and the
  derived rows in one snapshot.
- **A step** does one thing: if the source has changed since the image and one page
  holds every change, it applies them to the image's generation in one commit
  (`Updated`); otherwise it builds a new generation from the empty snapshot in pages
  (`Paged`) and its last page publishes it whole (`Published`); or it drops one page of
  a retired generation (`Dropped`). A source with no net change returns `Idle` with no
  commit. A diff is only ever taken from the exact source version the image holds; when
  that version can no longer be read, the step rebuilds.
- **`apply` and `drop`** are plain functions with an `inert` `env`, like every `run`
  body, because the store may abort and rerun a body. `apply` must leave the derived
  state determined by (generation, key, value).
- **Rebuild.** `rebuild` records a request; requests made while a build is running
  give one more build after it publishes.
- **Outside effects.** A body cannot call out. Write an outbox row in `apply`, keyed by
  (binding, generation, `at`, key), and have a separate worker call out and delete the
  row once acknowledged: delivery is at least once, and the receiver deduplicates by
  that key. A projector delivers net changes; to see every event, tail an append-only
  table or Log keyed by sequence.
- Laws: L72 `projector_converges`, L73 `projector_bounded` (STORE-SPEC v2.60 §1.14).
