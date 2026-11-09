# Storage

**Status: partly shipped.** This part states the contract of a place's store. The
implementation is converging on it:

- **Shipped and unchanged here:** the key encoding (STORE-6, `std@1/keys`); typed tables'
  create, get, put, delete and range, row epochs, storage identity and rename history
  (STORE-5, STORE-7, STORE-9); the output gate over one durable version (STORE-2); the
  call-stream decisions as `store.call` (STORE-12); and recording of a store's backend
  requests (STORE-10). `packages/s3@1` implements object-storage requests and grants.
- **Shipped, removed by this part, and deleted at the cutover:** closure bodies for
  `store.transact` and `store.call`; `store.begin`, `store.begin_with`, `store.commit`,
  `store.rollback`, `Txn`, `Isolation` and `Conflict()`; the `Backend[S]` record and its
  layout of numbered manifest, log and snapshot objects with fence objects, under which
  no object is ever removed; `StoreConfig.durability`; the `StopAfterWrites` seam and
  `Injected()`; `store.sequence`. The [standard library](std-library.md) lists the shipped
  operations until then. The history format (STORE-11) replaces that layout.
- **Planned (their names do not resolve yet):** everything else in this part.
- **Open designs,** where this part states the contract and the declarations are still to
  be written: the serving primitive behind `db@1` (STORE-12); the declarations of the
  authority seam and its grant (STORE-11); the wire format of the subscription feed and
  the client ingress (STORE-16, STORE-17); and the declaration of narrowing (STORE-3).
- Items marked **Reserved** have no implementation owner.

A **store** is one strictly serializable, ordered key-value database: a **history** of
commits, each made by one recorded transaction. A **place** is a store together with the
code that runs in it; it is the unit of ownership (one application, or one tenant of an
application), and nothing but runner calls crosses between places
([RUN-4](runners.md#run-4-calls)). Programs change a store only by transactions, which
are named top-level definitions holding nothing but a scoped `Tx`, their call information
and data. A transaction's outcome is released only once every commit it observed is
durable at the level its release requires.

The layers are fixed. A **backend** keeps the history and immutable objects, fences
writers and reports durability (STORE-11). The **engine**, `std@1/store`, orders and runs
transactions over byte keys, tracks every access and matches commits to subscriptions.
Everything else is a library over the engine: typed tables (`std@1/table`), indexes,
query languages, maintained views, workflows, migrations, verification and trial tools.
The engine knows no table. No core operation, TIR or LIR construct is added for stores:
`Tx` is the scoped capability of [MODE-6](ownership.md#mode-6-scoped-capabilities).

### STORE-1: Stores

```text
fn store.open(a: Authority, config: StoreConfig) -> Result[Store, StoreError]
fn store.close(take s: Store) -> Result[(), StoreError]
```

`store.open` opens or creates the store kept by the authority `a`
([STORE-11](#store-11-engine-and-storage-seam)) and claims it as its writer: it recovers
the newest state the history holds, takes the next **writer epoch**, and returns a
`Store`. `Store` is an ordinary affine opaque type; it exports no copying or sending
bound, and every operation that changes it takes it `mut`. One `Store` value is the
store's **head**: its only writer. A later claim of the same authority, in this process
or any other, fences the earlier head, whose next commit fails and which closes with
`Fenced()`. Other runs and hosts reach a store only through its head
([STORE-12](#store-12-transactions-at-a-runner)) or by following its history
([STORE-20](#store-20-one-history-many-machines)).

`store.close` waits until every commit is durable at the strongest level the backend
declares and releases the in-memory state; a store dropped without `close` loses only
commits that were not yet durable. Holding a `Store` grants nothing outside its
authority.

`StoreConfig` holds the settings of the engine provider, `db.local@1`. A setting that can
decide an outcome, a limit above all, is recorded in the store when it is created and is
the same for every run of every body; an opener cannot change it
([STORE-11](#store-11-engine-and-storage-seam)).

### STORE-2: Transactions

A transaction is a top-level definition ([RUN-3](runners.md#run-3-entries)) of one of two
shapes, with `A`, `R` and `E` `serializable`:

```text
fn(mut tx: Tx, call: CallInfo, a: A) -> Result[R, E]   // a transaction entry
fn(tx: Tx, call: CallInfo, a: A) -> Result[R, E]       // a query entry
```

```text
fn store.transact[A: serializable, R: serializable, E: serializable](mut s: Store, f: Entry[fn(mut tx: Tx, call: CallInfo, a: A) -> Result[R, E]], a: A) -> Result[R, TxFailure[E]]
fn store.query[A: serializable, R: serializable, E: serializable](snap: Snapshot, f: Entry[fn(tx: Tx, call: CallInfo, a: A) -> Result[R, E]], a: A) -> Result[R, TxFailure[E]]
```

There is no other way to run code against a store. A closure is never a transaction: a
transaction is identified by its **call record**, the definition path, code hash and
runtime contract revision of its entry, its call information
([STORE-4](#store-4-call-information)) and its argument bytes, and a call record can be
recorded, sent and run again ([STORE-10](#store-10-recording),
[STORE-15](#store-15-evaluation-and-re-execution)).

**The outcome rule.** A transaction's outcome, its writes and any failure it propagates
are those of one run of its body, with its call information, on the state left by the
commits before it in the store's history. The store may run a body any number of times,
on any prefix of the history, and may stop a run it will discard; it runs none when it
returns a retained outcome ([STORE-12](#store-12-transactions-at-a-runner)). No discarded
run is observable. The store chooses the order. A program never observes a conflict and
never retries: there is no conflict outcome.

1. The **deciding run** sees every commit before it and its own earlier writes.
2. `Ok(v)`: the writes commit at the next commit version. `Error(e)`: the writes are
   discarded and the outcome is `Error(Aborted(e))`. A language failure in the body
   discards the writes and propagates ([FAIL-2](failure.md#fail-2-boundary)); nothing is
   committed.
3. The outcome is released by the output gate below.

A query entry receives a borrowed `Tx`, so it cannot write
([STORE-3](#store-3-transaction-scope)); `store.query` runs it at a snapshot's version
([STORE-8](#store-8-versions-snapshots-and-forks)) and commits nothing. Its gate version
is the snapshot's.

`store.transact` performs no cancellation check
([CONC-3](concurrency.md#conc-3-cancellation)): a transaction started from a cancellable
scope is checked by its caller. Only the served form returns `Canceled(cause)`, before
any run and with no effect. A commit that exceeds a limit returns `Limit(..)` with no
effect. A commit whose durability cannot be decided
([STORE-11](#store-11-engine-and-storage-seam)) returns `Unknown(..)`: the store closes,
and whether the commit survived is decided when the store is next opened, never guessed.

Running one body at a time, in commit order, is how `db.local@1` meets the outcome rule.
It is that provider's cost model, not part of the contract.

#### Visibility and the output gate

Commits on one store are numbered by consecutive **commit versions**. The store's
**visible version** `V` is the highest commit applied to the head's state. A backend
declares its **durability levels** ([STORE-11](#store-11-engine-and-storage-seam)); for
each level `L`, the **durable version** `D(L)` is the highest commit version such that
it and every lower one are durable at `L`. No `D(L)` decreases or exceeds `V`, and a
stronger level's is never ahead of a weaker one's.

```text
fn store.durable(mut s: Store, ticket: Ticket, at: Level) -> Result[(), StoreError]
```

- **Visibility.** A deciding run observes every commit up to `V`, durable or not.
- **Output gate.** A transaction's **gate version** `g` is its own commit version when it
  committed, and the version it observed otherwise (a query, an abort or a refused
  limit). Its outcome, of any kind, leaves the place only when `D(L) >= g` for the level
  `L` its release requires: the level the caller waits for, and never one below the
  **floor** the deployment sets for the place. Every release is gated the same way: a
  reply, a subscription batch ([STORE-16](#store-16-subscriptions)), an outgoing call and
  a wake ([STORE-18](#store-18-runs-at-a-place)).
- **Crash.** A failure that level `L` survives loses no commit at or below `D(L)`. Every
  transaction that observed a lost commit has `g` above it, so no outcome released at `L`
  depends on lost state.

`store.durable` returns once `D(at)` reaches the ticket's version, advancing the queued
group if needed. `Level` is the level type the store's backend declares; a `Ticket` names
the commit version of one transaction.

### STORE-3: Transaction scope

`Tx` is a scoped capability ([MODE-6](ownership.md#mode-6-scoped-capabilities)): it cannot
be returned, stored, captured by an escaping closure or sent to another task. Reads
borrow a `Tx` and writes take it `mut`, because a borrowed value cannot collect a write
set without a core primitive; a body that receives a borrowed `Tx` therefore cannot
write. An entry's parameters are its `Tx`, its `CallInfo` and `serializable` data, and
its result is `serializable`, so a body holds no authority but its `Tx`, keeps nothing,
returns no authority, performs no effect outside the store and cannot open a nested
transaction ([CAP-3](capabilities.md#cap-3-restriction)). That is what lets the store run
a body again ([STORE-2](#store-2-transactions)).

**Narrowing.** A `Tx` can only be narrowed, as every capability can
([CAP-1](capabilities.md#cap-1-authority)): to a set of key ranges, each of a
named table or of raw keys, and to read-only access. A body passes the narrowed `Tx` to
the code it calls. An access outside it changes nothing and returns `OutOfScope()`; the
reads a library makes of its own reserved rows on behalf of an in-scope operation are
within it. Code that holds only a narrowed `Tx` cannot reach its parent's rows, so an
entry that narrows to its caller's tenant before calling anything else cannot leak
another's.

Every read and write a body makes goes through its `Tx` and is tracked
([STORE-14](#store-14-tracked-access)). Whether a read may wait on storage is the engine
provider's: under `db.local@1` no table operation suspends or is a cancellation point,
so once a body starts its run ends only by its own outcome, and a long body delays every
later transaction, which the store's limits bound.

### STORE-4: Call information

```text
CallInfo   // opaque: copyable, data, key, inert, sendable; minted only by a store's head
fn call_info.caller(c: CallInfo) -> String
fn call_info.at(c: CallInfo) -> i64
fn call_info.seed(c: CallInfo) -> u64
```

A store's head mints one `CallInfo` for each transaction it orders, records it in the
transaction's call record, and passes it to the body as its `call` parameter. Because
only a head constructs one, a body can trust it as the place's statement of:

- `caller`: the **principal** the place admitted. For a runner caller it is `run:`
  followed by the 32 lowercase hexadecimal digits of the run instance
  ([RUN-11](runners.md#run-11-linked-tapes)) for an in-process or child-process caller,
  or the caller name the host's configuration assigns to an authenticated remote caller.
  For a client it is the principal the place derived from the token it verified at
  ingress ([STORE-17](#store-17-clients)). It is the empty string for a transaction the
  head runs itself.
- `at`: the commit-order time, signed Unix nanoseconds from the head's clock. It never
  decreases in commit order on one store, and is 0 when the head has no clock.
- `seed`: eight bytes the head fixes when it orders the transaction, by default derived
  from the store identity and the transaction's place in the order and never from state
  another run of the body could see differently. The default is predictable and never
  protects a secret; a head may draw it from `random.system@1` instead.

The recorded call information is the deciding run's. `CallInfo` is the only way time,
randomness or caller identity enter a body, and it does so as recorded data.

### STORE-5: Tables

```text
Table[K, V]   // opaque: copyable, data, key, inert, sendable; holds a table name
RangeBound[K] = Unbounded() | Included(K) | Excluded(K)
ScanOrder     = Ascending() | Descending()
```

A `Table[K, V]` is inert data naming a table and its expected key and value types; it
carries no authority, and every operation on it takes a `Tx`. Every operation requires
`K: key + serializable` and `V: serializable`.

```text
fn table.named[K: key + serializable, V: serializable](name: String) -> Table[K, V]
fn table.create[K: key + serializable, V: serializable](mut tx: Tx, t: Table[K, V]) -> Result[(), TxError]
fn table.drop[K: key + serializable, V: serializable](mut tx: Tx, t: Table[K, V]) -> Result[(), TxError]
fn table.get[K: key + serializable, V: serializable](tx: Tx, t: Table[K, V], key: K) -> Result[Option[V], TxError]
fn table.put[K: key + serializable, V: serializable](mut tx: Tx, t: Table[K, V], key: K, value: V) -> Result[(), TxError]
fn table.delete[K: key + serializable, V: serializable](mut tx: Tx, t: Table[K, V], key: K) -> Result[bool, TxError]
fn table.range[K: key + serializable, V: serializable](tx: Tx, t: Table[K, V], from: RangeBound[K], to: RangeBound[K], limit: u64, order: ScanOrder) -> Result[List[(K, V)], TxError]
fn table.history(tx: Tx, name: String) -> Result[List[SchemaEntry], TxError]
```

- `table.named` checks nothing; a table name is valid when it is nonempty UTF-8 of at most
  255 bytes, contains no NUL, and does not begin with `hirsel.`, the namespace reserved
  for the store's own tables.
- `create` makes an empty table whose schema history holds one entry for `K` and `V`;
  `drop` removes the table, its rows and its history. There is no implicit creation.
- `get` returns a fresh owned copy of the value stored under `key`, or `None`. `put`
  inserts or replaces. `delete` removes the row and returns whether it existed.
- `range` returns the rows whose keys lie between `from` and `to` in key order
  ([STORE-6](#store-6-key-encoding)), ascending or descending, at most `limit` of them; a
  caller pages by passing `Excluded(last key)`. Rows are fresh owned data.
- `history` returns the named table's schema history ([STORE-9](#store-9-schema-history-renames-and-migrations)).

Each operation checks, in order: argument validity (a valid name; `limit` of at least 1),
else `InvalidInput()`; access (a write through a read-only `Tx`, else `ReadOnly()`; an
access outside a narrowed `Tx`, else `OutOfScope()`); the table exists (or, for `create`,
does not), else `NoTable()` or `TableExists()`; the table's current key and value schemas match `K` and `V`
([STORE-7](#store-7-rows-and-storage-identity)), else `Schema(..)`; a write to a table
under migration, else `Migrating()`; and the configured limits, else `Limit(..)`. A failed
operation changes nothing, and the transaction continues; the body decides whether to
abort. Tables carry no grants of their own: a body limits what the code it calls can
reach by narrowing its `Tx` ([STORE-3](#store-3-transaction-scope)).

`std@1/table` is ordinary Hirsel code over the store's byte keys. It keeps its catalog and
rows under keys beginning with `hirsel.`, the namespace of the engine and its libraries:
the byte operations `store.tx_get`, `store.tx_put`, `store.tx_delete` and `store.tx_range`
neither read nor write a key in it ([FACT-1](contracts.md#fact-1-contracts)), so no
program observes how a library encodes its rows.

```hirsel
@serializable
distinct type Sku(v: u64)

@serializable
type Reservation { sku: Sku, count: u32 }

@serializable
type StockError = OutOfStock(Sku) | Db(TxError)

fn stock() -> Table[Sku, u32] {
  table.named("stock")
}

fn reserved() -> Table[Sku, u32] {
  table.named("reserved")
}

pub fn take_stock(mut tx: Tx, call: CallInfo, req: Reservation) -> Result[Reservation, StockError] {
  let stocked = table.get(tx, stock(), req.sku) ? Db
  let have = match stocked { Some(v) => v, None => 0 }
  if have < req.count { return Error(OutOfStock(req.sku)) }
  table.put(mut tx, stock(), req.sku, have - req.count) ? Db
  let held = table.get(tx, reserved(), req.sku) ? Db
  let prior = match held { Some(v) => v, None => 0 }
  table.put(mut tx, reserved(), req.sku, prior + req.count) ? Db
  Ok(req)
}

pub fn reserve(mut s: Store, req: Reservation) -> Result[Reservation, TxFailure[StockError]] {
  store.transact(mut s, take_stock, req)
}
```

### STORE-6: Key encoding

Table keys are stored in the **key encoding**, `hirsel.key.v1`, a schema-directed encoding
whose unsigned byte-lexicographic order is the key order. It is separate from the
canonical codec, whose little-endian integers and length prefixes do not sort
([SER-2](serialization.md#ser-2-canonical-codec)). `std@1/keys` implements it
(`keys.encode`, `keys.decode`), and libraries use it for keys they build themselves, such
as secondary indexes kept as tables.

| Schema constructor | Encoding |
|---|---|
| `bool` | `00` for false, `01` for true |
| `u8` to `u128` | big-endian, exactly the type's width |
| `i8` to `i128` | big-endian two's complement with the sign bit inverted, exactly the type's width |
| `f32`, `f64` | the IEEE bits, big-endian, after the total-order transform: a value whose sign bit is set has every bit inverted, any other has only its sign bit flipped; the NaN is the canonical one ([SER-2](serialization.md#ser-2-canonical-codec)) |
| `char` | the scalar value as a big-endian `u32` |
| `String`, `Bytes` | each byte in order, with `00` written as `00 FF`; then `00 01` |
| unit | nothing |
| tuple, record | the fields in declaration order |
| variant, including core `Option` and `Result` and `Packed[T]` | the arm's declaration ordinal as a big-endian `u32`, then its fields in order |
| `box T`; an opted-in transparent nominal or distinct type | the payload's encoding |
| `Array[T]`, `List[T]` | for each element, `01` then the element; then `00` |
| `Inline[T, n]` | the `n` elements in order |

Every encoding is self-delimiting given the schema, so a tuple's encoding is the
concatenation of its fields'. **Key order** is defined by the same structure: integers and
`char` numerically; floats in IEEE total order, so `-0` precedes `+0` and the canonical NaN
follows every other value; `false` before `true`; `String` and `Bytes`
byte-lexicographically with a proper prefix first (the order of the
[SEM-5](semantics.md#sem-5-maps) operators); tuples and records lexicographically by field;
variants by arm ordinal and then payload (so `None` precedes every `Some`); and sequences
lexicographically with a proper prefix first. For all values `a` and `b` of one schema, `a`
precedes `b` in key order exactly when the encoding of `a` precedes the encoding of `b`
bytewise. A table key satisfies `key`, which excludes floats
([GEN-1](generics.md#gen-1-bounds)), so floats occur only in keys a library encodes itself.

`Map`, `Set` and `Simd` have no key encoding: the codec's `Set` order is not the key order,
so encoding them as sorted sequences would mislead. A key schema containing one is
STORE-6 at the use that instantiates the table operation. Decoding a stored key validates
it completely (escapes, terminators, `bool`, `char` and UTF-8 validity, float canonicity,
tags and trailing bytes); a stored key that does not decode is `Storage(Corrupt())`.

### STORE-7: Rows and storage identity

A row's value is stored as a `u32` little-endian schema epoch followed by the canonical
codec payload of the value, with no envelope; the table's schema history holds the
descriptors once ([STORE-9](#store-9-schema-history-renames-and-migrations)).

A table's schema is identified by **names and shape**: the package, module, type, field
and variant names of every nominal type it reaches, and its structure, with every package
version text excluded. The **storage descriptor** of a type is its concrete schema
descriptor ([SER-2](serialization.md#concrete-schema-descriptor)) with the exact version
text of every Nominal node replaced by the empty text atom. A schema matches a stored one
when their storage descriptors are equal after record fields are matched **by name**:

- a package version change that keeps names and shape needs nothing;
- reordering a record's fields needs nothing: fields are matched by name, and a stored row
  is read in its stored field order;
- a name that differs (a package, module, type, field or variant renamed) refuses with
  `Schema(Renamed(..))`, which names the stored and the expected spelling as a suggested
  rename, until the table's history declares that rename
  ([STORE-9](#store-9-schema-history-renames-and-migrations));
- any other difference (a field added, removed or retyped, an arm added or removed or
  reordered) refuses with `Schema(KeyMismatch())` or `Schema(ValueMismatch())` until a
  migration changes the table's schema.

Storage identity, like a descriptor digest, establishes no nominal type equality and
grants nothing. Reading a row checks that its epoch is one the table's history maps to the
current schema and decodes the payload against the operation's `V` with every SER-2
validation, without loading a type, a constructor or a decoder from the store. A row that
fails either check is `Storage(Corrupt())`.

### STORE-8: Versions, snapshots and forks

```text
Version    // opaque: copyable, data, key, inert, sendable; ordered
Snapshot   // opaque: copyable, inert, sendable; holds no authority
fn store.snapshot(s: Store, at: Level) -> Snapshot
fn store.at(a: Authority, v: Version) -> Result[Snapshot, StoreError]
fn store.fork(from: Authority, v: Version, to: Authority) -> Result[(), StoreError]
```

A **version** names one commit version of one store. It is an opaque ordered token:
versions of one store compare, and nothing else about them is observable; in particular
no program learns how many commits a store has made.

A **snapshot** is the immutable state of a store at one version. `store.snapshot` takes
one at `D(at)` and waits for nothing, so what a query reads from it depends on no commit
that a failure level `at` survives could lose; releasing that result at a stronger level
waits for that level like any outcome ([STORE-2](#store-2-transactions)). `store.at` opens the state at a retained version from the store's authority without
claiming it: a read "as of" that version, available to any holder of the authority, on
any machine. A query runs against a snapshot on any task, in parallel with the store's
transactions. Under `db.local@1` taking a snapshot of the head's state costs O(1).

**Forks.** `store.fork` creates, under the authority `to`, a new store whose history
begins as `from`'s history up to version `v`. The fork has its own identity and its own
writer epochs, and shares the immutable objects of its origin; it copies none. A fork is
never merged back. What the origin's history had promised to send or to wake (outgoing
calls and triggers, [STORE-18](#store-18-runs-at-a-place)) stays inert in the fork until
its head is granted the runners to act on it, so a fork never acts as its origin.

**Retention.** Which versions a store retains is store state, recorded in its history by
Retain commits ([STORE-11](#store-11-engine-and-storage-seam)): a floor version, a set of
named **pins**, and the checkpoints the retained versions rely on. A retained version
keeps what reading it, forking it and re-executing from it need: a checkpoint at or
below it, the groups from that checkpoint on and the code their call records name.
Reclamation frees only groups and nodes that no retained version and no fork reaches, and
only after the Retain commit that released them is durable.

### STORE-9: Schema history, renames and migrations

```text
SchemaEntry     = { epoch: u32, key_schema: String, value_schema: String, change: SchemaChange }
SchemaChange    = Created() | Renamed(from: String, to: String) | Migrated(definition: String, code: String)
MigrateError[E] = Failed(E) | Collision() | Table(TxError) | Canceled(CancelCause)

fn table.rename(mut tx: Tx, name: String, from: String, to: String) -> Result[(), TxError]
fn table.migrate[J: key + serializable, A: serializable, K: key + serializable, B: serializable, E: serializable](mut s: Store, from: Table[J, A], to: Table[K, B], f: Entry[fn(key: J, old: A) -> Result[(K, B), E]], batch: u64) -> Result[u64, MigrateError[E]]
```

Every table keeps its **schema history**: one entry per schema it has had, numbered from
epoch 0, each with the SHA-256 (64 lowercase hexadecimal digits) of the key and value
storage descriptors and the change that produced it. The last entry is current. Entries are
never edited or removed while the table exists.

**Renames.** `table.rename` declares, once, that the spelling `from` (a full nominal path,
or a field or variant name qualified by its type's path) is now spelled `to` in the named
table's schemas. It appends an entry whose descriptors are the current ones with the
rename applied, and rewrites no row: rows keep their epoch, and reads map the old
spelling through the history. A rename that does not occur in the current schema is
`InvalidInput()`.

**Migrations.** Any other change of a table's key or value schema is made only by
`table.migrate`, whose `f` is a pure top-level function ([RUN-3](runners.md#run-3-entries))
from an old row to a new one. `from` and `to` must name the same table, and `batch` must be
between 1 and the store's `range_rows` limit, else `Table(InvalidInput())`. Then:

1. A table whose current schema already matches `K` and `B` returns `Ok(0)`. Otherwise its
   current schema must match `J` and `A`, else `Table(Schema(..))`.
2. One transaction marks the table as migrating, with the target descriptors, the identity
   of `f` (its definition path and code hash, [RUN-8](runners.md#run-8-identity-and-arrival))
   and a cursor. From then on writes to the table return `Migrating()`; reads through
   `from` continue.
3. Successive transactions each read up to `batch` rows after the cursor in key order,
   apply `f` to each in order, write the results into a shadow table under the new
   schema, and advance the cursor. Cancellation is checked between these transactions.
4. One final transaction replaces the table's rows with the shadow's, appends the new
   schema entry, clears the mark, and the call returns `Ok(n)` with the number of rows
   migrated.

If `f` returns `Error(e)`, two rows map to the same new key, or the scope is cancelled,
one transaction discards the shadow and the mark, and the call returns `Failed(e)`,
`Collision()` or `Canceled(cause)`; the table is unchanged. A failure in `f` propagates and
leaves the migration pending; a later `table.migrate` with the same `from`, `to` and `f`
identity resumes it from its cursor, any other one returns `Table(Migrating())`, and
opening the store discards every pending migration. Key migrations are allowed, since `f`
maps the key too. Tooling may propose migrations for additive changes; the store applies
none on its own.

A migration is the only mechanism that maps stored data from one schema to another. It
maps every typed table alike, the tables that hold parked runs included
([STORE-18](#store-18-runs-at-a-place)). When a change of code needs several tables to
change schema together, their final publications are part of one activation
([STORE-19](#store-19-activation)).

### STORE-10: Recording

**Call records.** Every commit records its ordered write set of puts and deletes on byte
keys and values, whose SHA-256 is its write-set digest, and the call record of the
transaction that made it, with the outcome it returned ([STORE-2](#store-2-transactions)), with its call stream and position when
it arrived on one ([STORE-12](#store-12-transactions-at-a-runner)). A commit the engine
makes on its own behalf records which engine operation made it. The history therefore
states, for every change, what changed and which call, under which code, changed it.
How long call records are retained is store state
([STORE-8](#store-8-versions-snapshots-and-forks)).

Recovery is physical: it applies recorded write sets and never runs a body
([STORE-11](#store-11-engine-and-storage-seam)). Call records serve re-execution
([STORE-15](#store-15-evaluation-and-re-execution)), never recovery, so no stored state
depends on old code remaining runnable.

**Backend requests.** The requests a store makes of its backend are capability requests,
recorded as [determinism](determinism.md#recording-and-resuming) specifies, with the exact
bytes read. A recording therefore holds every byte the store loaded, including the rows
any transaction read, and is as secret-bearing as the store. Replay re-runs the engine
and every body against the recorded completions; it never reaches a real backend, and it
refuses a request that differs from the recorded one in any byte. The recording level
never changes what a transaction does.

At the `durable` level ([RUN-14](runners.md#run-14-recording-levels)) a commit's
durability point is the advance of the history that carries it, journaled like any
request. A crash between that request's `Prepared` and `Completed` records leaves it in
doubt, and recovery settles it by reading the history: an advance is one group whose
digest the writer computed before it sent it, so the history either holds that group or
holds another in its place. That is a provider recovery procedure
[recording and resuming](determinism.md#recording-and-resuming) allows.

### STORE-11: Engine and storage seam

A store's **backend** is a provider of the **authority** seam
([RUN-13](runners.md#run-13-provider-seams)), chosen by the host. An `Authority` is the
capability for one store's history and objects. A place's meaning never depends on its
backend: the backend decides only how long a release waits, what each failure can lose,
and which other machines can reach the store.

**The contract.** An authority provides exactly these:

| Operation | Contract |
|---|---|
| Claim | Become the writer at the next writer epoch. Every earlier writer's next advance fails with `Fenced()`. The fence is part of the history, never a separate object that reclamation could remove. |
| Advance | Conditionally extend the history from version `n` with the payloads of the next commits. It succeeds only for the current writer and only when the history ends at `n`. It is whole or nothing. |
| Read | Read the history from a version, and read an object. Reading needs no claim. |
| Put | Create an immutable object, only if its name is free; an existing object with equal bytes is success. |
| Durability | Declare the level type, a closed set of levels each documented by the failures it survives, and report each level's durable version, which never decreases. |
| Retention | Record the floor and the pins ([STORE-8](#store-8-versions-snapshots-and-forks)) in the history, and reclaim only what no retained version needs. |

An advance has one of four results: it happened; the writer was fenced; it was refused
before it had any effect; or it is **undecided** (a lost response, or a barrier that
failed after the advance became visible). An undecided advance is settled by reading the
history, never guessed. If it cannot be settled, the waiting transaction returns
`Unknown(..)` and the store closes.

How a backend represents an advance is its own: one conditional append to a log, synced
once for the group; or a data object followed by a conditional replacement of a head
object. There is no second way to extend a history, and a commit exists exactly when the
history holds it.

**History and derived objects.** The history is **authoritative**: only the head writes
it. Every other object (a checkpoint of the state at a version, an index part) is
**derived**: its bytes are a function of the history alone, in one canonical encoding,
with no writer identity. Any machine that reads the history may put a derived object,
and two that derive the same object write equal bytes. The history records the runtime
contract revision in force beside every code hash it names. Their bytes are declared
under **History format** below.

**Levels.** A backend that keeps nothing across a crash declares a single level that
survives nothing. A local log declares a level that survives a process crash and not the
loss of its disk. An object-storage backend declares the levels its service provides, for
example one zone or one region. A backend that acknowledges at a fast level and publishes
to a stronger one later declares both, and each `D(L)` reports its own progress. There is
no level every backend must offer ([RUN-13](runners.md#run-13-provider-seams)).

**Group commit.** Commits apply to the head's state at once, raising `V`, and queue. A
queue is advanced as one group when an outcome awaits its durability or a group bound in
the configuration is reached, never on a timer. **The head never holds a key across a
backend request:** ordering and running a transaction wait on no advance, no object read
and no other machine, so a contended key is served at memory speed whatever the backend.

**Limits.** `key_bytes` 4096, `value_bytes` 1048576, `transaction_bytes` 16777216 and
`range_rows` 10000 by default. They are store state, recorded by the Genesis commit and
bounded by what its backend can hold in one advance (**Bounds** below). A refused limit has no effect and is
definite, never `Unknown(..)`. A deterministic bound on the work one body may do, equal
on every engine, is **Reserved**.

A store must survive, by detecting and refusing, never by returning wrong data: the loss
of anything not yet durable, torn and garbage writes, lost or ambiguous advances, and
corruption on read. A simulated backend injects each of these, with refused requests,
lost responses, racing claims and virtual latency, and runs exactly the same engine; it
is the one fault path.

The engine provider is `db.local@1`: one head on one machine, every key and value in
memory, bodies run one at a time. Those are its costs, and so are the promises that a
snapshot is O(1) and that no read suspends. Reading key ranges from derived objects on
demand, so that a store may exceed memory, changes cost and no outcome.

```text
fn store.compact(mut s: Store) -> Result[(), StoreError]
```

`store.compact` puts the nodes of a checkpoint at the durable version's group end that are
not already present, and records its descriptor by a Checkpoint commit, so recovery
applies only the commits after it. A follower may put the same nodes; only the head
records the commit.

#### History format

Every integer is fixed-width little-endian. `H(x)` is SHA-256. A **byte string** is a
`u32` length followed by that many bytes. There is one format version, named by the
magic at the start of every record; a reader that meets another magic refuses the open
with `Storage(Corrupt(..))` naming the magic and the object, removes nothing and writes
nothing. A format change is a new magic and a cutover, never a second reader.

**Groups.** The history is a chain of **groups**, each the commits of one advance:

```text
group  = "HSHIST01" prev:[32] first:u64 epoch:u64 defs:u32 count:u32 def* commit*
def    = path:bytes code:[32] revision:bytes
commit = kind:u8 body writes
writes = n:u32 { op:u8 key:bytes [value:bytes if op = 1] }*
```

`prev` is the **digest** `H(group)` of the previous group, all zeros for the first;
`first` is the commit version of its first commit (versions start at 1); `epoch` is the
writer epoch of the head that wrote it. The definition table names each distinct entry
the group's commits ran (definition path, code hash, runtime contract revision,
[RUN-8](runners.md#run-8-identity-and-arrival)) once, and commits refer to it by index.
A write is a put (`op` 1) or a delete (`op` 2); keys strictly ascend, so a key occurs
once. The SHA-256 of a commit's `writes` bytes is its write-set digest; it is derived,
never stored. A group is valid only if every field decodes, nothing follows its last
commit, `first` is one above the previous group's last version, `epoch` is at least the
previous group's, and every index and op is in range. The digest chain is the history's
integrity: a reader that trusts a digest trusts every group before it.

Commit kinds:

| Kind | Body after `kind` | Records |
|---|---|---|
| 1 Transaction | `def:u32 at:i64 seed:u64 caller:bytes stream:u8 [identity:[32] position:u64 if stream = 1] args:bytes outcome:bytes` | The call record ([STORE-2](#store-2-transactions)): the entry, the call information ([STORE-4](#store-4-call-information)), the call stream and position it arrived on ([STORE-12](#store-12-transactions-at-a-runner)), the argument bytes and the canonical bytes of the `Result[R, TxFailure[E]]` it returned. |
| 2 Engine | `op:u8 payload:bytes` | A commit the engine makes on its own behalf, one of the operations below. An unknown `op` is corruption. |
| 3 Run | `run:[16] n:u32 { kind:u8 payload:bytes }*` | The journal records of a run at the place ([STORE-18](#store-18-runs-at-a-place)), with the kinds and payloads of the [durable journal](determinism.md#durable-journal). |

Every kind ends with its `writes`. Engine operations and their payloads:

| Op | Payload |
|---|---|
| 1 Genesis | `identity:[32]`, the limits `key_bytes value_bytes transaction_bytes range_rows` and the checkpoint node bounds `node_min node_mean node_max`, each `u32`. It is commit 1 and nowhere else. |
| 2 Claim | `epoch:u64 attempt:[16]`: a claim, one above the previous epoch, with the claimant's random attempt identity. It is the last commit of its group, and that group and every later one carry its epoch until the next claim. |
| 3 Checkpoint | A checkpoint descriptor (below). |
| 4 Retain | The whole retention state ([STORE-8](#store-8-versions-snapshots-and-forks)): `floor:u64`, `n:u32` pins `{ name:bytes version:u64 }` sorted by name, at most 1024 with names of at most 255 bytes, then `m:u32` checkpoint descriptors that retained versions rely on. |
| 5 Fork | `identity:[32] origin:[32] at:u64`: the first commit of a fork, at version `at + 1`. Its group's `prev` is the digest of the origin's group that holds `at`, and that group's commits above `at` are not the fork's. |
| 6 Stream | `op:u8 identity:[32]`: a fence (1) or purge (2) of a call stream ([STORE-12](#store-12-transactions-at-a-runner)). |
| 7 Library | `name:bytes payload:bytes`: a step a library records as its own, a migration's above all ([STORE-9](#store-9-schema-history-renames-and-migrations)). |

**Checkpoints.** A checkpoint is the state at a group's last version as a tree of
derived **nodes**, each named by the SHA-256 of its bytes:

```text
node  = "HSNODE01" level:u8 count:u32 entry*
leaf  = key:bytes value:bytes                         // level 0
index = first:bytes child:[32] entries:u64            // level > 0
```

Entries of a node strictly ascend by key (an index entry by its child's first key), and
an empty state is one empty leaf. Leaves are cut by content: after entry `e`, once the
node holds at least `node_min` bytes, when `u32le(H(key)[0..4]) * node_mean <
size(e) * 2^32`, and always before an entry that would take it past `node_max`; index
levels are cut the same way over their own entries until one root remains. Equal state
gives equal nodes on every machine, a node shared by two versions or two forks is one
object, and a change rewrites only the nodes whose range it touched and their ancestors.
The defaults are 256 KiB, 1 MiB and 4 MiB; `node_max` is at least the largest entry the
limits allow and at most the backend's object cap. A **descriptor** is
`version:u64 digest:[32] root:[32] height:u8 entries:u64 bytes:u64`: the checkpoint's
version (always a group's last), the digest of that group, the root node and the tree's
shape. A checkpoint counts only once a Checkpoint commit after its version records its
descriptor, so the chain authenticates it; its nodes are put before that commit.
Validating one reads its nodes from the root, checks each against the digest its parent
named, the keys' order and the descriptor's counts, and requires that the first group
read after it has `prev` equal to the descriptor's digest.

**The head record.** Each backend keeps one mutable record naming the history's end:

```text
head = "HSHEAD01" identity:[32] epoch:u64 attempt:[16] end:u64 digest:[32]
       checkpoint:descriptor locator:bytes retain:bytes origin:u8 [identity:[32] at:u64] sum:[32]
```

`sum` is the SHA-256 of everything before it. `end` and `digest` name the last group's
version and digest, `checkpoint` the newest recorded checkpoint, `retain` the payload of
the newest Retain commit, and `locator` where the group after the checkpoint starts. On
object storage the head record is the object every advance replaces conditionally, so
its `end` is the history's end. A local log rewrites it atomically (write, sync, rename,
sync the directory) at a claim, a checkpoint, a Retain commit and a segment change; its
`end` is then a lower bound that the log must reach, which detects the loss of a synced
suffix up to it. Until a store's first checkpoint the descriptor is all zeros, and
recovery starts from the first group.

**Local log.** Groups are appended to segment files in the
framed-file envelope of the [durable journal](determinism.md#durable-journal): magic `hirsel-history 1\n`,
identity the store's identity, generation the version of the segment's first commit,
one frame of kind 1 per group with sequences from 0, whose body digest is the group's
digest, so a group is hashed once. A segment starts at the first group after a
checkpoint, so earlier segments are reclaimed whole. One advance is one append and one
`fdatasync`.

**Object storage.** A group is the object `h/` followed by its first version as 20
decimal digits, `-` and its digest in lowercase hexadecimal, put only if free; then the
head record is replaced conditionally on the one it read. A node is `n/` followed by its
hexadecimal digest. A group object no head record's chain reaches, left by a writer
fenced or undecided before its head replace, is not history and is reclaimed. A listing
only finds candidates: every object is checked against the digest that names it.

**Bounds.** `transaction_bytes` bounds one encoded commit, its call record and outcome
included, so whether a transaction fits depends only on its run. Opening refuses a store
whose `transaction_bytes` plus 8 KiB of group and definition overhead exceeds the
largest advance its backend accepts, and a group is cut before it would exceed that, so
every commit the limits admit fits in one advance. The frame's `u32` length bounds a
local group at 4 GiB.

**Recovery.** Opening reads, in order:

1. the head record, and refuses unless its sum, magic and identity hold;
2. the checkpoint it names, nodes level by level, every leaf in parallel, validated as
   above;
3. the groups after the checkpoint: on a local log from the locator by sequential reads;
   on object storage by listing `h/` from the checkpoint's version and reading every
   candidate in parallel, then following `prev` back from the group whose digest the head
   record names to the checkpoint's digest; other candidates are orphans;
4. each group in order, checking its chain fields and applying its write sets.

Only the last frame of the last local segment can be **torn**: an incomplete final
frame, which recovery cuts. Anything else that fails is **corruption**: a bad digest,
sum, chain field or decoding, a missing referenced object, or a log that ends below the
head record's `end`. Corruption refuses the open with
`Storage(Corrupt(..))` naming the object, and removes nothing. A crash may tear or lose a
suffix no level had reported durable, and recovery ends the history before it. When no
intact state exists the open refuses rather than guess. Recovery never runs a body. The
claim of the new head follows recovery.

**Costs.** A local advance copies each commit's bytes once into a reusable group buffer,
hashes the group once, and makes one write and one `fdatasync`; framing adds 141 bytes
per group. A commit adds about 40 bytes plus its caller, argument, outcome and, per
write, 9 bytes plus the key and value. On object storage an advance is one put and one
conditional replace. Groups are written once and never rewritten; a checkpoint writes
only the nodes it does not share with the previous one, and the head record is the only
object rewritten. Recovery reads the head record, the checkpoint's nodes and the groups
after it: for `N` commits since the checkpoint, their bytes once, in about one group per
advance. `db.local@1` records a checkpoint when the history since the last one exceeds
the state's size and at least 64 MiB, so recovery reads at most about twice the state.

### STORE-12: Transactions at a runner

A place serves its store's entries to callers as runner calls
([RUN-4](runners.md#run-4-calls)), through the provider `db@1`:

```text
fn runner.transact[A: serializable, R: serializable, E: serializable](r: Runner, f: Entry[fn(mut tx: Tx, call: CallInfo, a: A) -> Result[R, E]], a: A) -> Result[R, RunError[TxFailure[E]]]
fn runner.query[A: serializable, R: serializable, E: serializable](r: Runner, f: Entry[fn(tx: Tx, call: CallInfo, a: A) -> Result[R, E]], a: A) -> Result[R, RunError[TxFailure[E]]]
```

`runner.transact` and `runner.query` name `Tx`, `CallInfo` and `TxFailure`, which
`std@1/store` declares, so they are declared beside them. The run that holds a `Store`
accepts an arrived entry after the arrival checks of
[RUN-8](runners.md#run-8-identity-and-arrival), orders it, runs it as
[STORE-2](#store-2-transactions) says and releases its outcome through the gate. A query
runs at a durable version on any machine that follows the store
([STORE-20](#store-20-one-history-many-machines)). A place serves only the entries on its
allowlist; no caller reaches a store's rows any other way.

The store's side of a transact call is one request on a call stream:

```text
CallRequest    = { stream: Bytes, epoch: u64, position: u64, digest: Bytes, acknowledged: u64 }
CallFailure[E] = Tx(TxFailure[E]) | Denied() | Rejected()
fn store.call[A: serializable, R: serializable, E: serializable](mut s: Store, req: CallRequest, f: Entry[fn(mut tx: Tx, call: CallInfo, a: A) -> Result[R, E]], a: A) -> Result[R, CallFailure[E]]
```

`store.call` is `store.transact` as one request on a call stream, decided as the contract
below says. `Denied()` and `Rejected()` are the runner's `NotRun(Denied())` and
`NotRun(Rejected())`; `Tx(..)` is the transaction's own outcome.

- **Call streams.** Each caller sends its transact calls to a place on an ordered call
  stream, so the state a store keeps grows with the number of callers, not of calls. A
  stream belongs to one caller at one store: a caller run, with identity
  `H("hirsel.stream.v1", caller name, run instance)`, or a client session
  ([STORE-17](#store-17-clients)). The store keeps one row per stream under the reserved
  key `hirsel.streams/` followed by the stream identity: its `epoch`, its `applied`
  position, its `acknowledged` position, and the request digest and outcome bytes (`Ok`,
  `Aborted` or `Limit`) of each position above `acknowledged` and at most `applied`. A
  stream's identity and positions are logical: they do not change when the head moves to
  another machine or the store to another backend.
- A request carries the stream identity, the caller's epoch, a position, the request
  digest and the highest position whose outcome the caller has made durable. Positions
  start at 1 and are consecutive, and a caller has at most one request in flight per
  stream; a request that ends in `NotRun` leaves its position to the caller's next
  request. The store decides each request in the transaction that would run it: an epoch
  below the row's is `NotRun(Denied())`; a position at or below `acknowledged`, or above
  `applied + 1`, is `NotRun(Rejected())`; a position above `acknowledged` and at most
  `applied` returns its recorded outcome when the digest is equal and is
  `NotRun(Rejected())` when it differs; position `applied + 1` runs the body, and the same
  commit records the outcome, sets `applied`, raises `acknowledged` and drops the outcomes
  at or below it. A request whose epoch exceeds the row's raises it in the commit that
  decides the request, a duplicate's included. An outcome that commits no writes is
  recorded in a transaction of its own that writes only the stream row. The row is one of
  the commit's writes and counts against the store's limits: when the body's writes and
  the row recording their outcome exceed them, the outcome is `Limit(..)`, recorded by a
  commit of the row alone; when not even that row fits (the caller holds too many outcomes
  unacknowledged), the request is refused `Limit(..)` without being recorded and its
  position stays `applied + 1`.
- **Fencing.** A durable caller's executor holds an epoch for each stream, recorded in its
  journal header. A live continuation first commits, on each store the run used, a fence
  request that raises the stream's epoch and returns its `applied` position and retained
  outcomes. A `Prepared` position at or below `applied` then has its recorded outcome, and
  one above it proves the transaction never committed, so a transact is never in doubt. A
  caller ends its stream by acknowledging its last position; an administrator may purge
  abandoned streams, which the store records, and a purged stream's requests are
  unresolvable thereafter rather than "never run".
- A store that is fenced or whose durability cannot be decided ends every unreleased call
  with `InDoubt(Abandoned())`, which the caller's stream resolves.

**Across places.** Places exchange only runner calls. A transact call is exactly once at
its place; no call makes two places commit together. A library protocol over call
streams, in which each place first commits a hold on the keys it will change and a
coordinator then commits one decision, is **Reserved**. Its one requirement of the engine
is that the store, which chooses the order, runs a transaction that touches a held key
range only after the hold is decided.

### STORE-13: Failures

```text
TxFailure[E] = Aborted(E) | Canceled(CancelCause) | Storage(StoreError) | Limit(TxLimit)
             | Unknown(StoreError)
TxError      = NoTable() | TableExists() | Schema(SchemaError) | Migrating() | ReadOnly()
             | OutOfScope() | InvalidInput() | Limit(TxLimit) | Storage(StoreError)
SchemaError  = KeyMismatch() | ValueMismatch() | Renamed(stored: String, expected: String)
StoreError   = Storage(FileError) | Refused(FileError) | Corrupt(String) | Fenced() | Closed()
TxLimit      = KeyBytes() | ValueBytes() | TransactionBytes() | RangeRows()
```

Variant ordinals in the order written, `serializable` (`TxFailure[E]` when `E` is).
`TxFailure` is the outcome of a transaction: `Aborted(e)` when the body returned
`Error(e)`; `Canceled(cause)` before any run; `Storage(..)` and `Limit(..)` when the
commit could not be made, with no effect; and `Unknown(..)` when the commit was attempted
and its durability could not be decided (STORE-11), which never claims an effect or its
absence. `TxError` is the outcome of one operation inside a body. `Storage(e)` is an
error from the backend, a full medium included; `Refused(e)` that the backend refused a
request before it had any effect; `Corrupt(name)` that the named part of the history or
object failed a checksum, decoding or structural check; `Fenced()` that a later writer
claimed the store; `Closed()` that the store closed earlier. Every one closes the store.
No outcome reports a conflict or an exhausted retry ([STORE-2](#store-2-transactions)).

### STORE-14: Tracked access

The **access set** of one run of a body is defined once, over byte keys:

- every key the run read, whether or not it held a value;
- every range the run scanned, recorded as the interval it actually consumed: from the
  bound it started at to the last key it examined, or to the range's end when the scan
  exhausted it, with its direction, so a scan cut short by its limit depends on no key
  past the cut;
- every key the run wrote or deleted;
- every reserved row the engine or a library read or wrote on the run's behalf, a call
  stream's row and a table's catalog entry among them.

No operation reads or writes a store without entering the access set. The set is sound:
a commit that changes none of what a run's access set read cannot change that run's
outcome. Three uses depend on that soundness and use the set as defined: accepting a run
made on an older version ([STORE-20](#store-20-one-history-many-machines)), subscriptions
([STORE-16](#store-16-subscriptions)) and caches of query results. Uses that only advise,
such as fetching ahead of a run and placement statistics, may approximate it.

### STORE-15: Evaluation and re-execution

**Evaluation** runs one call record against the state at one version and commits
nothing. It yields the outcome bytes, the write set and the access set
([STORE-14](#store-14-tracked-access)). It is the engine's one way to run a body: a
commit is an evaluation at the head followed by an advance of the history, and a query is
the evaluation of a query entry. A place loads the code a call record names by its hash
([RUN-13](runners.md#run-13-provider-seams)). A body can perform no effect
([STORE-3](#store-3-transaction-scope)), so an evaluation needs no confinement and leaves
no trace.

**Re-execution** is evaluation of a recorded call under a chosen code selection, the
recorded hash or a candidate's, with its inputs taken from the record: for a transaction
or a query, the state at the version it observed; for an effectful run, its tape
([determinism](determinism.md#recording-and-resuming)). It reports, per call, one of
three results: **same** (equal outcome bytes and write set), **different**, or
**unknown**, when the comparison cannot be made (the entry's signature changed, the
recorded runtime contract is unavailable, or the run asks for an outside answer its tape
does not hold). Unknown is never reported as same or as different.

Replay, verification, trials, simulation and tests are uses of re-execution, not separate
mechanisms. A **verifier** re-executes commits under their recorded code; a difference
there is always a defect, in the engine or in the determinism of a body, so a verifier
may gate. Every other use, a trial of a candidate revision or the reproduction of one
call, is a library or a tool over evaluation, versions and forks
([STORE-8](#store-8-versions-snapshots-and-forks)). This specification defines no gate on
differences between revisions.

### STORE-16: Subscriptions

A **subscription** registers a set of keys and key ranges with a store: the read part of
a query's access set ([STORE-14](#store-14-tracked-access)), or a range an entry granted.
For each commit, the engine's **matcher** compares the commit's write set with the
registered sets and notifies each subscription it touches, with one of two payloads:

- **stale**: the subscription's query must be evaluated again, at that commit's version;
- **the matched writes**: the puts and deletes that fell inside the registered set.

One mechanism serves every live read. A maintained view is a library that writes derived
tables, either in the transaction that changes their inputs or after it; subscribers
watch a derived table as they watch any other.

Delivery obeys one rule:

1. A session receives one batch per commit version, holding every notification that
   commit caused it, so all of a session's subscriptions advance together.
2. A batch is released by the output gate ([STORE-2](#store-2-transactions)), like any
   outcome.
3. There is no gap between the version a result was read at and the first commit its
   subscription is matched against.
4. A query whose new outcome bytes equal its last delivered ones delivers nothing.
5. Only commits cause notifications. The passage of time never does; a query that
   depends on time receives it as an argument.

The matcher runs after a commit is ordered and never while the head holds a key
([STORE-11](#store-11-engine-and-storage-seam)); any machine that follows the history can
run it ([STORE-20](#store-20-one-history-many-machines)).

### STORE-17: Clients

A **client** is a program outside the place's trust: a browser or an application. It is an
untrusted, partial follower of the store.

- **Calls.** A client calls the entries the place exports to clients and nothing else. It
  cannot name a key, a table or a query of its own.
- **Identity.** The place verifies a client's token at ingress and derives its principal,
  which every one of its calls carries as `call_info.caller`
  ([STORE-4](#store-4-call-information)). A capability never reaches a client
  ([CAP-5](capabilities.md#cap-5-capability-sealing)); what a client may do is decided by
  rows the place checks.
- **Authorization** is the code of the exported entries. Its reads are tracked like any
  others, so a subscription that depended on a membership row is notified when that row
  changes. An entry narrows its `Tx` to what its caller may reach
  ([STORE-3](#store-3-transaction-scope)) before it calls anything else. There is no rule
  language, and no row is readable except through an exported entry.
- **State.** A client holds the rows its subscriptions cover, each batch at one commit
  version ([STORE-16](#store-16-subscriptions)), together with what those rows cover, so
  that it can tell an absent row from one it does not hold. A client that reconnects
  resumes from its last version while the store retains it
  ([STORE-8](#store-8-versions-snapshots-and-forks)) and otherwise starts again.
- **Exactly once.** A client's transact calls travel on its own call stream
  ([STORE-12](#store-12-transactions-at-a-runner)), so a call queued while disconnected
  and sent again is decided once.
- **Prediction.** A client may evaluate a transaction entry on the rows it holds
  ([STORE-15](#store-15-evaluation-and-re-execution)) and show the result as provisional.
  It sends the call, never its writes; the place's own run decides the outcome, and the
  batch that carries it replaces the provisional result.
- Nothing reaches a client before the output gate releases it.

Typed client bindings are generated from the exported entries' signatures and schemas;
they are tooling, not part of this contract.

### STORE-18: Runs at a place

A place also runs effectful work: durable runs that call other places and outside
services, wait, and resume.

**One history.** The journal records of a durable run that belongs to a place
([durable journal](determinism.md#durable-journal)) commit in the place's history, in the
same commit as the run's writes, its call stream rows and its **outgoing intents**, the
calls and triggers it asks the place to issue. The `Prepared` and `Completed` barriers
around every outside effect remain: an intent is issued only after the commit that holds
it is released by the gate, and its completion is committed before the run observes it.
A run with no place keeps its own journal provider.

**One revision per run.** A run executes the one revision selected when it was admitted
([PROG-1](programs.md#prog-1-names)). A deployment bounds how long a run may stay open;
that bound is also how long a change of code waits for open runs to finish
([STORE-19](#store-19-activation)).

**Parks.** A wait that may outlast a run's bound is a **park**: the run ends by
committing a typed row of its state and a **trigger**, which names an entry by path,
never by code hash, and one condition: a deadline, the arrival of a message, or the
completion of a call. When the condition holds, the place admits a fresh run of the named
entry, on the code then active, with the row as its argument; the first commit of that
run consumes the trigger. Parked state and triggers are rows of typed tables
([STORE-7](#store-7-rows-and-storage-identity)), so they are read, migrated
([STORE-9](#store-9-schema-history-renames-and-migrations)) and checked at activation like
any other rows. A library may offer straight-line workflows over parks; a workflow whose
code changes while it is open is that library's to refuse.

### STORE-19: Activation

A place changes its code and its schemas by one commit, an **activation**. It is a
transaction like any other, so its call record states what it activated, with any
evidence its caller attached as argument bytes. One activation selects:

- the code revision the place admits new runs and transactions on;
- the schema root of every table whose schema changes, the tables of parked runs
  included, with the identity of each migration that produced it
  ([STORE-9](#store-9-schema-history-renames-and-migrations));
- for every run open under the outgoing revision, that it finished within the bound or
  is named as abandoned.

An activation commits only when the result is coherent:

- every row of every changed table decodes under the schema the activation selects;
- every parked row's trigger names an entry that exists in the selected revision and
  accepts that row's type, and every call a parked run awaits completes with a type its
  resume entry accepts ([STORE-18](#store-18-runs-at-a-place));
- no exported entry has changed its signature. A breaking change to an exported entry is
  a new entry under a new name; the old one stays until it is removed on purpose, because
  callers and clients that hold the old signature cannot be changed in the same commit;
- no outside effect of an open run is in doubt, unless the activation names it as
  abandoned.

A refused activation changes nothing. Places activate independently: a revision reaches
many places one activation at a time, and no activation spans two places.

### STORE-20: One history, many machines

No result of any program depends on which machine holds a store's head, on how many
machines follow its history, on where its objects are kept, or on which backend keeps
them ([DET-3](determinism.md#det-3-no-switches)). These change cost, latency and what a
failure can lose, and nothing else.

A store has one head, and the head is its only fenced role
([STORE-11](#store-11-engine-and-storage-seam)). No machine owns a key or a range of
keys: there is no partition a program declares, no assignment of keys to machines and no
commit protocol between owners. A **follower** is any machine that reads the history. It
applies write sets as recovery does, serves queries and subscriptions at durable versions
([STORE-16](#store-16-subscriptions)), puts derived objects, and may evaluate a
transaction ahead of the head.

The head may commit a run a follower made on an older version only when that run's
access set ([STORE-14](#store-14-tracked-access)) shows that no later commit wrote
anything it read. Otherwise the head runs the body itself. Either way the outcome is the
one [STORE-2](#store-2-transactions) defines, and no program observes which happened.
Routing work toward a machine that already holds the data it reads is a matter of cost
only.

Three properties keep this true and are part of the contract: a version reveals nothing
but order ([STORE-8](#store-8-versions-snapshots-and-forks)); time, randomness and
identity enter a body only as recorded call information
([STORE-4](#store-4-call-information)); and stream identities and positions are logical
([STORE-12](#store-12-transactions-at-a-runner)).
