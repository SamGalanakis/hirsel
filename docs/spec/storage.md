# Storage

**Status: partly shipped.** This part states the contract of a place's store. The
implementation is converging on it:

- **Shipped and unchanged here:** the key encoding (STORE-6, `std@1/keys`); typed tables'
  create, drop, get, put, delete and range, row epochs, storage identity, rename history
  and migrations (STORE-5, STORE-7, STORE-9); the call-stream decisions as `store.call`
  (STORE-12). `packages/s3@1` is an object-storage client; a store over S3 grants
  `authority.objects@1` the host provider `objects.s3@1`, which a host run admits and a
  simulated run (fixtures, the interpreter) refuses like `objects.dir@1`.
- **Shipped:** transactions as entries: `store.transact`, `store.query` and `store.call`
  run an `Entry` through `runner.apply_mut` on decoded argument bytes, with `CallInfo`
  minted before the run and a failure in the run ending as `Faulted` (STORE-2, STORE-4,
  STORE-12, STORE-13). A query entry borrows its `Tx`, and reads record through the
  borrow. The engine over the authority seam: `store.open(a, config)`, the history format
  with claim, Genesis and Checkpoint commits, `store.compact` writing the checkpoint tree,
  `store.ticket`, `store.durable` and `store.snapshot` at a level (STORE-1, STORE-2,
  STORE-8, STORE-11). Stores written by the earlier numbered-object layout are not
  opened.
- **Format, at the cutover:** each commit records its call (entry index, call
  information, argument bytes and outcome bytes), and the definition table's `revision`
  is empty until the runtime contract revision is exposed to std. A stream still keeps
  its retained outcomes in its head row; the per-position outcome rows of STORE-12 land
  in the change after the cutover, and stores written before it are not opened.
- **Planned (their names do not resolve yet):** everything else in this part.
- Items marked **Reserved** have no implementation owner.

A **store** is one strictly serializable, ordered key-value database: a **history** of
commits, each made by one recorded transaction. A **place** is a store together with the
code that runs in it; it is the unit of ownership (one application, or one tenant of an
application), and nothing but runner calls crosses between places
([RUN-4](runners.md#run-4-calls)). Programs change a store only by transactions, which
are named top-level definitions holding nothing but a scoped `Tx`, their call information
and data. A transaction's outcome is released only once every commit it observed is
durable at the level its release requires.

The layers are fixed. An **authority** keeps the history and immutable objects, fences
writers and reports durability (STORE-11). The **engine**, `std@1/store`, orders and runs
transactions over byte keys, tracks every access and matches commits to subscriptions.
Everything else is a library over the engine: typed tables (`std@1/table`), indexes,
query languages, maintained views, workflows, migrations, verification and trial tools.
The engine knows no table. The authority is a host provider seam like a directory's; no
TIR or LIR construct and no store-specific core operation is added: `Tx` is the scoped capability of
[MODE-6](ownership.md#mode-6-scoped-capabilities). Beside the authority seam, the engine
is std code over three core additions, which concern entries and places rather than
stores: `runner.apply` and `runner.apply_mut` run a named entry in the current run as a
failure boundary, `runner.entry_id` states an entry's identity
([RUN-3](runners.md#run-3-entries)), and a place's `Inbox` hands its head the calls that
arrived for it ([STORE-12](#store-12-transactions-at-a-runner)).

### STORE-1: Stores

```text
fn store.open(a: Authority, config: StoreConfig) -> Result[Store, StoreError]
fn store.close(take s: Store) -> Result[(), StoreError]
```

`store.open` opens or creates the store kept by the authority `a`
([STORE-11](#store-11-engine-and-storage-seam)) and claims it as its writer: it appends a
claim entry with the next **writer epoch** to the history, recovers the newest state the
history holds, and returns a `Store` that keeps `a`. Of openers that race, exactly one claim succeeds and
every other open returns `Fenced()`. `Store` is an ordinary affine opaque type; it exports no copying or sending
bound, and every operation that changes it takes it `mut`. One `Store` value is the
store's **head**: its only writer. A later claim of the same authority, in this process
or any other, fences the earlier head: its next advance of the history returns
`Fenced()`, and it closes. Other runs and hosts reach a store only through its head
([STORE-12](#store-12-transactions-at-a-runner)) or by following its history
([STORE-20](#store-20-one-history-many-machines)).

`store.close` waits until every commit is durable at the strongest level the authority
declares and releases the in-memory state; a store dropped without `close` loses only
commits that were not yet durable. Holding a `Store` grants nothing outside its
authority.

```text
StoreConfig = { group_commits: u64, group_bytes: u64, limits: StoreLimits }
StoreLimits = { key_bytes: u64, value_bytes: u64, transaction_bytes: u64, range_rows: u64 }
```

`StoreConfig` holds the settings of the engine provider, `db.local@1`: the group bounds
of [STORE-11](#store-11-engine-and-storage-seam) and the limits. A setting that can decide
an outcome, a limit above all, is recorded in the store when it is created and is the same
for every run of every body; an opener cannot change it, and the limits it passes to an
existing store are ignored. Durability is not a setting: each wait names its level
([STORE-2](#store-2-transactions)), and faults come only from a simulated authority.

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
transaction is identified by its **call record**, the identity of its entry
(`runner.entry_id`: definition path, signature fingerprint and code hash,
[RUN-3](runners.md#run-3-entries)), the runtime contract revision in force, its call
information ([STORE-4](#store-4-call-information)) and its argument bytes, and a call
record can be recorded, sent and run again ([STORE-10](#store-10-recording),
[STORE-15](#store-15-evaluation-and-re-execution)).

**Argument and outcome bytes.** The **argument bytes** are the canonical codec encoding of
`a` ([SER-2](serialization.md#ser-2-canonical-codec)), the same bytes a runner call sends;
the store encodes them once, at the call, and records them. Every run receives fresh owned
data decoded from them, never the caller's `a`, because a codec round trip canonicalizes
NaNs and `Set` order and re-execution decodes from the record. The **outcome bytes** are
the canonical encoding of the `Result[R, TxFailure[E]]` the call returned, which its call
record holds and a served call's end carries ([STORE-12](#store-12-transactions-at-a-runner)),
and a caller receives fresh owned data equal to decoding them, whichever form it called,
so a local and a served call of one entry return equal values
([RUN-5](runners.md#run-5-placement-independence)). An engine skips a decode, or computes
no outcome bytes, only where that is observably identical: for a schema with no float and
no `Set`, a local call passes `a` and returns the body's value directly, and encodes the
outcome only for a stream row or a reply.

**The outcome rule.** A transaction's outcome, its writes and any failure it propagates
are those of one run of its body, with its call information, on the state left by the
commits before it in the store's history. The store may run a body any number of times,
on any prefix of the history, and may stop a run it will discard; it runs none when it
returns a retained outcome ([STORE-12](#store-12-transactions-at-a-runner)). No discarded
run is observable. The store chooses the order. A program never observes a conflict and
never retries: there is no conflict outcome.

1. The **deciding run** sees every commit before it and its own earlier writes.
2. `Ok(v)`: the writes commit at the next commit version. `Error(e)`: the writes are
   discarded and the outcome is `Error(Aborted(e))`. A language failure `f` in the body
   discards the writes and the outcome is `Error(Faulted(f))`; nothing is committed. Every
   run is a failure boundary ([FAIL-2](failure.md#fail-2-boundary)), so the failure of a
   discarded run is discarded with it, and only the deciding run's reaches the caller.
3. The outcome is released by the output gate below.

A query entry receives a borrowed `Tx`, so it cannot write
([STORE-3](#store-3-transaction-scope)); `store.query` runs it at a snapshot's version
([STORE-8](#store-8-versions-snapshots-and-forks)) and commits nothing. Its gate version
is the snapshot's.

`store.transact` performs no cancellation check
([CONC-3](concurrency.md#conc-3-cancellation)): a transaction started from a cancellable
scope is checked by its caller. Cancellation is observed only before a transaction starts
and between transactions; once started, neither its body nor its commit's history `Io`
observes it, so a cancelled task's commit completes with its outcome and the store stays
open ([STORE-11](#store-11-engine-and-storage-seam)). Only the served form returns `Canceled(cause)`, before
any run and with no effect. A commit that exceeds a limit returns `Limit(..)` with no
effect. A commit whose durability cannot be decided
([STORE-11](#store-11-engine-and-storage-seam)) returns `Unknown(..)`: the store closes,
and whether the commit survived is decided when the store is next opened, never guessed.

Running one body at a time, in commit order, is how `db.local@1` meets the outcome rule.
It is that provider's cost model, not part of the contract.

#### Visibility and the output gate

Commits on one store are numbered by consecutive **commit versions**. The store's
**visible version** `V` is the highest commit applied to the head's state. Each authority
declares its own **durability levels** ([STORE-11](#store-11-engine-and-storage-seam)); for
each level `L`, the **durable version** `D(L)` is the highest commit version such that
it and every lower one are durable at `L`. No `D(L)` decreases or exceeds `V`, and a
stronger level's is never ahead of a weaker one's.

```text
Level  // core opaque: copyable, sendable; not data, no equality or order; `Level of a`
Ticket // opaque: copyable, data, inert, sendable
fn authority.levels(a: Authority) -> List[Level of a]
fn authority.level(a: Authority, name: String) -> Option[Level of a]
fn authority.floor(a: Authority) -> Level of a
fn level.name(l: Level) -> String
fn level.narrow(a: Authority, l: Level) -> Option[Level of a]
fn store.ticket(s: Store) -> Ticket
fn store.durable(a: Authority, mut s: Store, ticket: Ticket, at: Level of a) -> Result[(), StoreError]
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

An authority declares an ordered list of levels, weakest first, each a name and the
failures it survives; a later level survives every failure an earlier one survives, and
"stronger" and "below" mean later and earlier in that list. There is no level vocabulary
shared by every authority ([RUN-13](runners.md#run-13-provider-seams)). A `Level of a` is
one of the levels `a` declares: `authority.levels` lists them, weakest first,
`authority.level` resolves a name to one, and `authority.floor` is the deployment's floor. A level belongs to its authority: the
checker refuses a level of one authority where another's is required
([PROV-1](brands.md#prov-1-brands)). `Level` is `ordered of` its authority
([SEM-5](semantics.md#sem-5-order-and-maps)): levels of one authority compare by the
declared list, weaker before stronger, so `a < b` holds exactly when `b` is stronger, and a
comparison of levels whose authorities differ or are unknown is
[PROV-2](brands.md#prov-2-checking). A level kept where its brand cannot be written (a field, a list) is recovered
with `level.narrow`, which is `None()` for a level of another authority. A level travels
between places and hosts only as its name (`level.name`), which the receiving head
resolves with `authority.level`; a name the authority does not declare is `None()`, and
the store operation it was for returns `NoLevel()`, definite and without effect, never a
nearby level.

`store.ticket` names the store's visible version `V` in its history: a transaction's own
commit when taken right after it. A ticket carries the store identity, so it never names
a version of another history. `store.durable` takes the store's authority with it, so
`at` is checked to belong to `a` by the checker; a store that does not keep `a` returns
`NoLevel()`, as the level is not one of its authority's. It returns once `D(at)` reaches the ticket's
version, advancing the queued group if needed; a ticket of another store returns
`Foreign()`. Neither changes the store. Any other error closes the store and never claims
that the ticket's commit is lost: the next open settles it.

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

**Byte operations.** A body reads and writes byte keys through its `Tx`, with `RangeBound`
and `ScanOrder` as [STORE-5](#store-5-tables) declares them:

```text
fn store.tx_get(tx: Tx, key: Bytes) -> Result[Option[Bytes], TxError]
fn store.tx_range[A](tx: Tx, from: RangeBound[Bytes], to: RangeBound[Bytes], limit: u64, order: ScanOrder, take initial: A, step: fn(take A, Bytes, Bytes) -> (A, bool)) -> Result[A, TxError]
fn store.tx_put(mut tx: Tx, take key: Bytes, take value: Bytes) -> Result[(), TxError]
fn store.tx_delete(mut tx: Tx, take key: Bytes) -> Result[(), TxError]
```

`tx_get`, `tx_put` and `tx_delete` require a key outside the reserved namespace `hirsel.`
([FACT-1](contracts.md#fact-1-contracts)); `tx_range` passes over the keys in it as if they
were absent. They check, in order: argument validity (`limit` of at least 1), else
`InvalidInput()`; access, below; and the configured limits, else `Limit(..)`. `tx_range`
folds `step` over at most `limit` entries between `from` and `to` in `order`, starting
from `initial`. Each call gets the entry's key and value as views of the transaction's
state that last for that call, and returns the next accumulator and whether to go on;
the fold ends there, at the `limit`-th entry or at the far bound. `step` copies what it
keeps, and cannot write through the `Tx` while the fold reads it
([CLO-1](closures.md#clo-1-scoped)). It is the only range read through a `Tx`. A
failed operation changes nothing.

**Narrowing.** A `Tx` can only be narrowed, as every capability can
([CAP-1](capabilities.md#cap-1-authority)): to a **key scope**, a set of byte keys each
with an access, read or read and write.

```text
Access   = Read() | ReadWrite()
KeyScope   // opaque: copyable, data, inert; a set of byte keys, each with an Access
fn store.keys(from: RangeBound[Bytes], to: RangeBound[Bytes], access: Access) -> KeyScope
fn store.all(access: Access) -> KeyScope
fn store.join(a: KeyScope, b: KeyScope) -> KeyScope
fn store.narrow[R: inert](mut tx: Tx, scope: KeyScope, body: fn(mut sub: Tx) -> R uses {}) -> Result[R, TxError]
fn store.narrow_read[R: inert](tx: Tx, scope: KeyScope, body: fn(sub: Tx) -> R uses {}) -> Result[R, TxError]
```

- `store.keys` is the keys between `from` and `to` outside the reserved namespace, each
  with `access`; `store.all` is every key, the reserved namespace included. `store.join`
  is the union, each key with the stronger of its two accesses. A key scope is data
  without authority: it grants nothing until a `Tx` is narrowed to it. Two key scopes
  are equal when they hold the same keys with the same accesses.
- **Scope of a `Tx`.** The `Tx` a transaction or query entry receives has the scope
  `store.all(ReadWrite())`. `store.narrow` and `store.narrow_read` run `body` once with a
  `Tx` of the same transaction whose scope is exactly `scope`, and return `Ok` of its
  result. They return `OutOfScope()` without running `body` unless `scope` is
  **within** `tx`'s scope: every key it holds, `tx`'s scope holds with an access at
  least as strong (`ReadWrite()` is stronger than `Read()`). So a narrowing of a
  narrowing is within both, and the relation is reflexive and transitive.
- **What the body holds.** `body` is `uses {}` and its result `inert`
  ([CAP-3](capabilities.md#cap-3-restriction)), so it cannot capture the parent `Tx`
  and cannot keep or return the narrowed one
  ([MODE-6](ownership.md#mode-6-scoped-capabilities)): code that holds only the narrowed
  `Tx` cannot reach its parent's rows. The narrowed `Tx` reads the same state and its
  writes are the transaction's writes. `store.narrow_read` takes a borrowed `Tx`, as a
  query entry holds, and gives `body` a borrowed one, which cannot write.
- **Access.** Every operation checks each key before it reads or writes it, and a range
  before it reads any of it: a key or range outside the scope returns `OutOfScope()`, and
  a write to a key the scope holds only with `Read()` returns `ReadOnly()`. A range is
  within the scope when every key in it between its bounds is held, except that a byte
  range needs none of the reserved keys it passes over. The check reads nothing, so a
  refused access neither depends on nor reveals a row outside the scope.
- **Libraries.** The engine makes no exception for a library's own rows: a library
  reads and writes them through the same check, and gives the keys an operation needs
  in the scopes it builds. `std@1/table` builds two:

```text
fn table.scope[K: key + serializable, V: serializable](t: Table[K, V], access: Access) -> KeyScope
fn table.scope_keys[K: key + serializable, V: serializable](tx: Tx, t: Table[K, V], from: RangeBound[K], to: RangeBound[K], access: Access) -> Result[KeyScope, TxError]
```

  `table.scope` holds the whole table with `access`: its rows, its **catalog entry**,
  the one reserved row a row operation reads about the table (the digests of its current
  storage descriptors, its current generation and which history entries its rows may
  name), its **history rows**, one reserved row per schema history entry, written only
  by `create`, `rename` and a migration's install, and read by `history` and by an
  operation whose types are not the table's current ones, and, read only, the store's
  **pending deletions** of its rows and history rows, which `create` checks so a
  generation is reused only once its earlier rows are gone. It reads
  nothing, so it serves a table that the body will create, and holds the rows under any
  generation; the operations reach only the current one
  ([STORE-7](#store-7-rows-and-storage-identity)). `table.scope_keys` holds the rows of
  the current generation whose keys lie
  between `from` and `to` with `access`, and the catalog entry and history rows with `Read()`, so that
  get, put, delete, range and history work on those rows and nothing can create, drop
  or rename the table. It reads the catalog entry through `tx` to encode the bounds, and
  fails as `table.get` does when that read fails. Neither holds any other table's catalog entry, so a narrowed
  `Tx` learns nothing of a table outside its scope, not even whether it exists.

A narrowing is computation inside one run of a body. It records nothing, and
re-execution computes it again ([STORE-15](#store-15-evaluation-and-re-execution)).
Accesses through a narrowed `Tx` enter the run's one access set
([STORE-14](#store-14-tracked-access)); `table.scope_keys`'s catalog read is tracked like
any other, and a refused access enters nothing. An entry that narrows to its caller's
rows before calling anything else cannot leak another caller's:

```hirsel
fn balances() -> Table[u64, u64] {
  table.named("balances")
}

fn owners() -> Table[u64, String] {
  table.named("owners")
}

@serializable
type Transfer { from: u64, to: u64, cents: u64 }

@serializable
type Refusal = NotYours(u64) | NoAccount(u64) | Insufficient(u64) | Db(TxError)

// Checks the owner row through the full Tx, then hands the move only the two balances.
pub fn transfer(mut tx: Tx, call: CallInfo, req: Transfer) -> Result[u64, Refusal] {
  if table.get(tx, owners(), req.from) ? Db != Some(call_info.caller(call)) {
    return Error(NotYours(req.from))
  }
  let mine = table.scope_keys(tx, balances(), Included(req.from), Included(req.from), ReadWrite()) ? Db
  let theirs = table.scope_keys(tx, balances(), Included(req.to), Included(req.to), ReadWrite()) ? Db
  store.narrow(mut tx, store.join(mine, theirs), fn(mut sub: Tx) -> Result[u64, Refusal] {
    move_cents(mut sub, req)
  }) ? Db
}

fn move_cents(mut tx: Tx, req: Transfer) -> Result[u64, Refusal] {
  let Some(have) = table.get(tx, balances(), req.from) ? Db else { return Error(NoAccount(req.from)) }
  let Some(other) = table.get(tx, balances(), req.to) ? Db else { return Error(NoAccount(req.to)) }
  if have < req.cents { return Error(Insufficient(have)) }
  table.put(mut tx, balances(), req.from, have - req.cents) ? Db
  table.put(mut tx, balances(), req.to, other + req.cents) ? Db
  Ok(req.cents)
}

// A query entry narrows its borrowed Tx to the caller's one balance, read only.
pub fn balance(tx: Tx, call: CallInfo, account: u64) -> Result[u64, Refusal] {
  if table.get(tx, owners(), account) ? Db != Some(call_info.caller(call)) {
    return Error(NotYours(account))
  }
  let only = table.scope_keys(tx, balances(), Included(account), Included(account), Read()) ? Db
  let read = store.narrow_read(tx, only, fn(sub: Tx) -> Result[Option[u64], TxError] {
    table.get(sub, balances(), account)
  }) ? Db
  match read ? Db {
    Some(cents) => Ok(cents),
    None() => Error(NoAccount(account)),
  }
}
```

In `move_cents`, a `table.get` of any third account returns `OutOfScope()`, as does a
`table.create(mut sub, balances())`, which needs `table.scope(balances(), ReadWrite())`.

**Cost.** The `Tx` holds its scope as sorted, disjoint intervals, adjacent ones of equal
access merged. Under `db.local@1` a key check on an unnarrowed `Tx` is one branch, and on
a narrowed one a binary search over its intervals, comparing the key in place; a range
check walks the intervals it overlaps. Neither allocates. `store.narrow` installs the
scope in the parent `Tx` for the duration of `body` and restores it, copying no state, no
write set and no access set; checking that `scope` is within the parent's is one merge
pass over both interval lists. A key scope allocates its intervals once, when it is
built.

Every read and write a body makes goes through its `Tx` and is tracked
([STORE-14](#store-14-tracked-access)). Whether a read may wait on storage is the engine
provider's: under `db.local@1` no table operation suspends or is a cancellation point,
so once a body starts its run ends only by its own outcome, and a long body delays every
later transaction, which the store's limits bound.

### STORE-4: Call information

```text
CallInfo   // opaque: copyable, data, key, inert, sendable; minted only by the engine
fn call_info.caller(c: CallInfo) -> String
fn call_info.at(c: CallInfo) -> i64
fn call_info.seed(c: CallInfo) -> u64
```

A store's head mints one `CallInfo` for each transaction it orders, before any run of it,
records it in the transaction's call record, and passes the same value to every run of
the body as its `call` parameter. Because only the engine constructs one, a body can
trust it as the place's statement of:

- `caller`: the **principal** the place admitted. For a runner caller it is `run:`
  followed by the 32 lowercase hexadecimal digits of the run instance
  ([RUN-11](runners.md#run-11-linked-tapes)) for an in-process or child-process caller,
  or the caller name the host's configuration assigns to an authenticated remote caller.
  For a client it is the principal the place derived from the token it verified at
  ingress ([STORE-17](#store-17-clients)). It is the empty string for a call made locally,
  through `store.transact`, `store.query` or `store.call`.
- `at`: the commit-order time, signed Unix nanoseconds from the head's clock. It never
  decreases in commit order on one store, and is 0 when the head has no clock.
- `seed`: eight bytes the head fixes when it orders the transaction, by default derived
  from the store identity and the transaction's place in the order and never from state
  another run of the body could see differently. The default is predictable and never
  protects a secret; a head may draw it from `random.system@1` instead.

The recorded call information is the deciding run's. `CallInfo` is the only way time,
randomness or caller identity enter a body, and it does so as recorded data.

A query is not ordered, so its call information holds only its caller: `at` and `seed`
are 0. A query's outcome is then a function of its entry, its argument, its caller and
the state at its version, and of nothing that changes when a commit leaves its access set
alone ([STORE-14](#store-14-tracked-access)), which subscriptions and result caches rely
on; a query that depends on time takes it as an argument
([STORE-16](#store-16-subscriptions)).

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
fn table.drop[K: key + serializable, V: serializable](mut s: Store, t: Table[K, V]) -> Result[(), TxFailure[TxError]]
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
  `drop` takes the store, not a `Tx`: one transaction removes the table's catalog entry,
  and its rows and history rows are then deleted in transactions that each fit the store's limits, so a
  table of any size can be dropped. It returns `Aborted(e)` for a refusal, with the error
  `e` below, and the store's failure otherwise; a crash before the catalog entry's removal
  commits leaves the table whole, and one after it leaves rows that the next open deletes
  ([STORE-7](#store-7-rows-and-storage-identity)).
- `get` returns a fresh owned copy of the value stored under `key`, or `None`. `put`
  inserts or replaces. `delete` removes the row and returns whether it existed.
- `range` returns the rows whose keys lie between `from` and `to` in key order
  ([STORE-6](#store-6-key-encoding)), ascending or descending, at most `limit` of them; a
  caller pages by passing `Excluded(last key)`. Rows are fresh owned data.
- `history` returns the named table's schema history ([STORE-9](#store-9-schema-history-renames-and-migrations)).

Each operation checks, in order: argument validity (a valid name; `limit` of at least 1),
else `InvalidInput()`; access to the table's catalog entry, read for `get`, `put`,
`delete`, `range` and `history` and written for `create`, `drop` and `rename`, else
`OutOfScope()` or `ReadOnly()` ([STORE-3](#store-3-transaction-scope)); the table exists
(or, for `create`, does not), else `NoTable()` or `TableExists()`; the table's current key and value schemas match `K` and `V`
([STORE-7](#store-7-rows-and-storage-identity)), else `Schema(..)`; access to the row key or
key range the operation names, else `OutOfScope()` or `ReadOnly()`; and the configured
limits, else `Limit(..)`. A failed
operation changes nothing, and the transaction continues; the body decides whether to
abort. Tables carry no grants of their own: a body limits what the code it calls can
reach by narrowing its `Tx` ([STORE-3](#store-3-transaction-scope)).

`std@1/table` is ordinary Hirsel code over the store's byte keys. It keeps its catalog and
rows under keys beginning with `hirsel.`, the namespace of the engine and its libraries:
the byte operations ([STORE-3](#store-3-transaction-scope)) neither read nor write a key
in it, so no program observes how a library encodes its rows.

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
[SEM-5](semantics.md#sem-5-order-and-maps) operators); tuples and records lexicographically by field;
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

A table's rows live under a **generation**, one byte after the table's encoded name in
every row key, followed by the encoded key ([STORE-6](#store-6-key-encoding)); within a
generation the byte order of row keys is the table's key order. The table's catalog entry
names its current generation, and every table operation takes it from the catalog entry it
already reads, so a read records the same access as without one. Only a migration writes a
second generation, the table's next, and installs it by pointing the catalog entry at it
([STORE-9](#store-9-schema-history-renames-and-migrations)). A generation no catalog
entry names is garbage: the store records it in the reserved space `hirsel.garbage/` in
the transaction that orphans it, deletes it in transactions that each fit the store's
limits, and finishes that deletion when it is opened after a crash.

A table's schema is identified by **names and shape**: the package, module, type, field
and variant names of every nominal type it reaches, and its structure, with every package
version text excluded. The **storage descriptor** of a type is its concrete schema
descriptor ([SER-2](serialization.md#concrete-schema-descriptor)) with the exact version
text of every Nominal node replaced by the empty text atom: the core library's
`codec.descriptor`, which the compiler emits as static data. A schema matches a stored one
when their storage descriptors are equal after record fields are matched **by name**:

- a package version change that keeps names and shape needs nothing;
- reordering the fields of a record in the value needs nothing: fields are matched by
  name, and a stored row is read in its stored field order;
- reordering the fields of a record in the key, at any depth, is a key change: a table has
  one key order, the byte order of its encoded keys, and a key record's fields are encoded
  in declaration order, so it refuses with `Schema(KeyMismatch())` like any other key
  change;
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
Version    // opaque: copyable, plain, inert, sendable; ordered of its store's authority
Snapshot   // opaque: copyable, inert, sendable; holds no authority
fn store.snapshot(a: Authority, mut s: Store, at: Level of a) -> Result[Snapshot of a, StoreError]
fn store.version(a: Authority, snap: Snapshot of a) -> Version of a
fn store.at(a: Authority, v: Version of a) -> Result[Snapshot of a, StoreError]
fn store.fork(from: Authority, v: Version of from, to: Authority) -> Result[(), StoreError]
fn store.retain(a: Authority, mut s: Store, floor: Version of a) -> Result[(), StoreError]
fn store.pin(a: Authority, mut s: Store, name: String, v: Version of a) -> Result[(), StoreError]
fn store.unpin(mut s: Store, name: String) -> Result[(), StoreError]
```

A **version** names one commit version of one store. It is an opaque ordered token,
`Version of a` for the store's authority `a` ([PROV-2](brands.md#prov-2-checking)):
versions of one store compare in commit order with `==` and `<`, and nothing else about
them is observable; in particular no program learns how many commits a store has made.
A store's comparison domain is its authority, so versions taken under one authority
compare across a close and reopen, and comparing versions of two authorities is PROV-2.
A version is neither `data` nor `key` and is never serialized or hashed.

A **snapshot** is the immutable state of a store at one version, together with the store
identity and the level it was taken at. `store.snapshot` takes one at `D(at)`
([STORE-2](#store-2-transactions)) and waits for nothing, so what
a query reads from it depends on no commit that a failure level `at` survives could lose.
Releasing that result at a stronger level waits for that level like any outcome, and
reading one's own write takes `store.durable` on its ticket and then a new snapshot; a
snapshot never advances. The head keeps the state at `V` and at each declared level's
`D(L)`, sharing equal states, so the snapshots it can take cost O(levels) retained states.
`store.snapshot` takes the store `mut` because it shares a state the head holds,
which it moves out and back, though it changes nothing observable.
`store.version` names a snapshot's version; it takes `a` by plain shared borrow to name
the comparison domain and exercises no authority. `store.at` opens the state at a retained version from the store's authority without
claiming it: a read "as of" that version, available to any holder of the authority, on
any machine. A query runs against a snapshot on any task, in parallel with the store's
transactions. Under `db.local@1` taking a snapshot of the head's state costs O(1).

**Forks.** `store.fork` creates, under the authority `to`, a new store whose history
begins as `from`'s history up to version `v`. The fork has its own identity and its own
writer epochs, and shares the immutable objects of its origin; it copies none. A fork is
never merged back. What the origin's history had promised to send or to wake (outgoing
calls and triggers, [STORE-18](#store-18-runs-at-a-place)) stays inert in the fork until
its head is granted the runners to act on it, so a fork never acts as its origin. A run
the fork continues takes a new run instance derived from the fork's store identity and
the origin's instance, so each of its call streams at another place
([STORE-12](#store-12-transactions-at-a-runner)) is a new stream: the fork never sends on
or fences a stream of its origin, and a call the origin had in doubt stays in doubt for
the fork, which can only abandon it.

**Retention.** Which versions a store retains is store state, recorded in its history by
Retain commits ([STORE-11](#store-11-engine-and-storage-seam)): a floor version, a set of
named **pins**, and the checkpoints the retained versions rely on. A retained version
keeps what reading it, forking it and re-executing from it need: a checkpoint at or
below it, the groups from that checkpoint on and the code their call records name.
`store.retain` raises the floor and never lowers it: a floor at or below the current one
changes nothing, and one above `D` of the strongest declared level waits for it.
`store.pin` and `store.unpin` set and remove a pin; a pin below the floor returns
`InvalidInput()`. Each is a commit the engine makes on its own behalf. Reclamation frees
only groups and nodes that no retained version and no fork reaches, and only after the
Retain commit that released them is durable
([STORE-11](#store-11-engine-and-storage-seam)).

### STORE-9: Schema history, renames and migrations

```text
SchemaEntry     = { epoch: u32, key_schema: String, value_schema: String, change: SchemaChange }
SchemaChange    = Created() | Renamed(from: String, to: String) | Migrated(entry: EntryId)
MigrateError[E] = Failed(E) | Faulted(Failure) | Collision() | Table(TxError) | Canceled(CancelCause)

fn table.rename(mut tx: Tx, name: String, from: String, to: String) -> Result[(), TxError]
fn table.migrate[J: key + serializable, A: serializable, K: key + serializable, B: serializable, E: serializable](within: Scope, mut s: Store, from: Table[J, A], to: Table[K, B], f: Entry[fn(c: (), key: J, old: A) -> Result[(K, B), E]], batch: u64) -> Result[u64, MigrateError[E]]
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
`table.migrate`, whose `f` is an entry ([RUN-3](runners.md#run-3-entries)) from an old row
to a new one. Its context is `()`: the context position states what an entry's body may
touch, and `()` gives it nothing, which is how the type records that a row function is
pure. `from` and `to` must name the same table, and `batch` must be between 1 and the
store's `range_rows` limit, else `Table(InvalidInput())`. Then:

1. A table whose current schema already matches `K` and `B` returns `Ok(0)`. Otherwise its
   current schema must match `J` and `A`, else `Table(Schema(..))`.
2. One transaction records the table's next generation
   ([STORE-7](#store-7-rows-and-storage-identity)) as garbage. That record is the
   migration's only recovery state: until the install, the catalog entry names the current
   generation, so a migration that stops deletes the next one and leaves the table as it
   was. The migration holds the store (`mut s`), so no other transaction runs until it
   returns.
3. Successive transactions each read up to `batch` rows after the last one migrated, in
   key order, apply `f` to each in order through `runner.apply(f, (), key, old)`
   ([RUN-3](runners.md#run-3-entries)), and write the results into the next generation
   under the new schema. Cancellation of `within` is checked between these transactions.
4. One transaction installs the migration whatever the table's size: it points the
   catalog entry at the next generation, appends the new schema entry
   `Migrated(runner.entry_id(f))`, the identity of `f` (its path, signature fingerprint
   and code hash, [RUN-8](runners.md#run-8-identity-and-arrival)), removes the next
   generation's garbage record, and records the replaced generation as garbage. The replaced rows are then deleted, and the
   call returns `Ok(n)` with the number of rows migrated.

No commit of a migration holds more than one batch of rows, so a migration is bounded by
no commit limit. If `f` returns `Error(e)`, fails (`runner.apply` returns
`Error(failure)`), two rows map to the same new key, `within` is cancelled, or a step's
commit exceeds the store's limits, the migration is discarded and the call returns
`Failed(e)`, `Faulted(failure)`, `Collision()`, `Canceled(cause)` or `Table(Limit(..))`;
the table is unchanged. A discard deletes the next generation, in transactions that each
fit the store's limits, and its last transaction removes the garbage record, so the
generation is reused only once its rows are gone. A crash leaves the table
either as it was or migrated: before the install commits, opening the store discards the
migration; after, it deletes the replaced generation. Key migrations are allowed,
since `f` maps the key too. Tooling may propose migrations for additive changes; the store
applies none on its own.

A generation number is reused: the generation after 255 is 0, a migration retried after a
discard writes the generation the discard deleted, and a table created after a drop starts
again at generation 0. A number is written again only once every row of its earlier life is
deleted. The store guarantees this: a generation becomes garbage in the transaction that
orphans it (a migration's next generation when the migration starts, until its install; the
replaced one at the install; the current one at a drop); the operation deletes it before it
returns; any failure of that deletion, a `Limit(..)` included, closes the store
([STORE-13](#store-13-failures)); and opening a store finishes every deletion before its
first transaction, or fails. So while a store is open no garbage remains but the next
generation of the migration that holds it. It is also checked where the number is chosen:
the transaction that starts a migration and `create` read the generation they are about to
write, and if a garbage record or any row remains under it they refuse with
`Storage(Corrupt(..))` and write nothing.

A migration is the only mechanism that maps stored data from one schema to another. It
maps every typed table alike, the tables that hold parked runs included
([STORE-18](#store-18-runs-at-a-place)). When a change of code needs several tables to
change schema together, their final publications are part of one activation
([STORE-19](#store-19-activation)).

### STORE-10: Recording

**Call records.** Every commit records its ordered write set of puts and deletes on byte
keys and values, whose SHA-256 is its write-set digest, and the call record of the
transaction that made it, with the outcome it returned ([STORE-2](#store-2-transactions)),
with its call stream, position and request digest when it arrived on one
([STORE-12](#store-12-transactions-at-a-runner)). A commit the engine
makes on its own behalf records which engine operation made it. The history therefore
states, for every change, what changed and which call, under which code, changed it.
How long call records are retained is store state
([STORE-8](#store-8-versions-snapshots-and-forks)).

Recovery is physical: it applies recorded write sets and never runs a body
([STORE-11](#store-11-engine-and-storage-seam)). Call records serve re-execution
([STORE-15](#store-15-evaluation-and-re-execution)), never recovery, so no stored state
depends on old code remaining runnable.

**Authority requests.** The requests a store makes of its authority are capability requests,
recorded as [determinism](determinism.md#recording-and-resuming) specifies, with the exact
bytes read. A recording therefore holds every byte the store loaded, including the rows
any transaction read, and is as secret-bearing as the store. Replay re-runs the engine
and every body against the recorded completions; it never reaches a real authority, and it
refuses a request that differs from the recorded one in any byte. The recording level
never changes what a transaction does.

At the `durable` level ([RUN-14](runners.md#run-14-recording-levels)) a commit's
durability point is the advance of the history that carries it, journaled like any
request. A crash between that request's `Prepared` and `Completed` records leaves it in
doubt, and recovery settles it by reading the history: every advance carries its
writer's claim and is one group whose digest the writer computed before it sent it, so
the history either holds that group or holds another in its place. That is a provider recovery procedure
[recording and resuming](determinism.md#recording-and-resuming) allows.

### STORE-11: Engine and storage seam

A store's history and objects are kept by an **authority**, a provider of the authority
seam ([RUN-13](runners.md#run-13-provider-seams)) that the host grants by name. An
`Authority` is the capability for one store's history and objects and nothing else. A
place's meaning never depends on its authority: the authority decides only how long a
release waits, what each failure can lose, and which other machines can reach the store.

```text
Authority      // opaque capability: copyable, sendable; not data
Claim          // opaque: affine, neither copyable nor sendable; valid for one authority
Advanced       = Advanced(u64) | Fenced() | Refused(AuthorityError) | Undecided(AuthorityError)
AuthorityError = Io(FileError) | Status(u16) | Unreachable()
SeamError      = Storage(AuthorityError) | Refused(AuthorityError) | Corrupt(String) | Fenced()
               | Format(Bytes, String)

fn io.authority(rt: Io, name: String) -> Result[Authority, GrantError]
fn authority.max_entry(a: Authority) -> u64
fn authority.claim(a: Authority) -> Result[Claim of a, SeamError]
fn claim.end(c: Claim) -> u64
fn claim.narrow(a: Authority, take c: Claim) -> Option[Claim of a]
fn authority.advance(a: Authority, mut c: Claim of a, from: u64, take entry: Bytes, at: Level of a) -> Advanced
fn authority.durable(a: Authority, mut c: Claim of a, at: Level of a) -> Result[u64, SeamError]
fn authority.read(a: Authority, from: u64, at: Level of a, max: u64) -> Result[List[Bytes], SeamError]
fn authority.get(a: Authority, name: String) -> Result[Option[Bytes], SeamError]
fn authority.put(a: Authority, name: String, take data: Bytes) -> Result[(), SeamError]
fn authority.reclaim(a: Authority, mut c: Claim of a) -> Result[(), SeamError]
```

`SeamError` is the seam's own failure, a core type; the engine reports each as the
`StoreError` of the same name, `Format(..)` as `Unsupported(Format(..))`
([STORE-13](#store-13-failures)). `claim.end` is the position the claim's holder last
extended the history to. A claim kept where its brand cannot be written (a field of the
store) is recovered with `claim.narrow`, which is `None()`, ending the claim, when it is
not `a`'s.

The engine, `std@1/store`, is the caller these operations serve. Holding an `Authority`
grants what holding the store's files grants: an entry written outside the engine is read
by recovery like any other and refused if it breaks the format.

**Cancellation.** No seam operation is a cancellation point
([CONC-3](concurrency.md#conc-3-cancellation)), under any provider: a task's `claim`,
`advance`, `durable`, `read`, `get`, `put` and `reclaim` run to their outcome whatever its
scope's cancellation, as a shielded drop's `Io` does, and no provider sees a cancellation.
A commit the engine has started therefore either completes or, when the scope was
cancelled before it started, never began; it is never cut between its entry and its
durability, and a cancellation never closes the store.

**Positions.** The history is a sequence of **entries** at positions 1, 2, and so on; its
**end** is the position of the last one, 0 for none. An entry is bytes to the authority.
What an entry holds (a commit group, a claim, a retention change) and how it is framed
and checked is the format's (below); commit versions are numbered inside entries, never by positions.

**Claim.** `authority.claim` makes its caller the history's only writer. It reads the
history to its end, settles a torn tail (see Recovery), and appends a **claim entry**
naming a fresh **claim identity**, drawn by the provider and recorded as a provider
outcome, and the next writer epoch. Of claims that race, exactly one succeeds; each other
returns `Fenced()` and appends nothing. The first claim of an empty authority mints the
**store identity**, which every later claim entry, ticket and frontier carries and which
no other history uses, a fork's included. The fence is the claim entry itself: there is
no fence object, so nothing that reclaims objects can remove one.

**Advance.** `authority.advance(a, c, from, entry, at)` puts `entry` at position
`from + 1`, whole or nothing, only if `c` is the claim of the newest claim entry and the
history ends at `from`, and returns once the entry is durable at `at`:

- `Advanced(end)`: the entry is in the history at `end` and durable at `at`.
- `Fenced()`: a later claim entry is in the history, and nothing was appended.
- `Refused(e)`: the request was refused before it had any effect.
- `Undecided(e)`: whether the entry is in the history could not be read, or the entry is
  in the history and the barrier for `at` failed. The provider settles a lost response
  itself by reading the history (its own claim at `from + 1` means it happened; a later
  claim means `Fenced()`) and returns `Undecided` only when it cannot read. A failed
  barrier is never retried into success.

After `Fenced()` or `Undecided(..)` the claim is dead: every later operation with it
returns `Fenced()`. The engine closes the store, a waiting transaction returns
`Unknown(..)` on `Undecided`, and the next claim settles what survived. A head has at most
one advance in flight; the groups behind it queue. A superseded head learns that it was
fenced at its next `advance`, `durable` or `reclaim`; it never released an outcome that
depends on a commit the history does not hold, because the gate waited for the advance.
There is no second way to extend a history, and a commit exists exactly when the history
holds it.

**Durability and reads.** `authority.durable(a, c, at)` returns the highest position
durable at `at`, issuing at most one barrier for every entry since the last.
`authority.read(a, from, at, max)` returns up to `max` entries after position `from`
that are durable at `at`. It needs no claim: followers and `store.at` read this way.
`authority.levels` lists the levels the provider declares, weakest first
([STORE-2](#store-2-transactions)); `authority.floor` is the deployment's floor for the
place, the declared level the grant names; `authority.max_entry` bounds the bytes of one
entry.

**Objects.** `authority.put` creates an immutable named object only if its name is free;
an existing object with equal bytes is success, and one with other bytes is
`Corrupt(name)`. `authority.get` reads one, `None` when it is absent. No object is ever
replaced. Object names are the format's.

**Reclamation.** `authority.reclaim` removes only what no retained version needs, and
decides it from the history alone: the floor and pins its retention entries record
([STORE-8](#store-8-versions-snapshots-and-forks)), durable at the strongest declared
level. It removes entries and derived objects that only versions below the floor need,
and objects written under a claim older than the newest that no entry names. A floor
never decreases and a claim is never reused, so a reclaim that runs from a stale reading
of the history removes nothing that a newer reading keeps, a successor's checkpoint
included. Recovery never depends on a listing: it may use one only to fetch in parallel
what it then reaches by name. An incomplete listing leaves garbage for a later reclaim and
never loses state.

**Providers.** The run configuration grants authorities by name
([determinism](determinism.md#authority-grants)); each entry names its provider and
floor.

| Provider | History | Levels |
|---|---|---|
| `authority.memory@1` | host memory | `memory` (survives no failure) |
| `authority.log@1` | segment files and a lock file in a granted directory | `process` (the process's crash: the entry is written to the operating system), `host` (the operating system or power failing, the disk surviving: the entry is synchronized) |
| `authority.objects@1` | a head object and immutable segment objects in an object-storage entry | the one level its grant names and states, such as `zone` or `region` |
| `authority.sim@1` | either representation, simulated | as its grant states |

- **Local log.** An advance takes the lock file's open-file-description lock (`flock` or
  `F_OFD_SETLK`; never a process-associated `fcntl` lock, which closing any descriptor of
  the file releases), compares the active segment's length with the length the writer's
  own last append left, appends the entry with one `write`, and releases the lock; a
  different length means another claim appended, and the advance returns `Fenced()` with
  no effect. It then issues one `fdatasync` when `at` is `host`. A new segment
  adds a directory sync when it is created. A claim takes the same lock, truncates only a
  torn tail, appends its claim entry and synchronizes. A reader at `host` notes the
  file's length, synchronizes the file and returns entries up to that length, so it never
  returns an entry a host crash could lose. A failed `fdatasync` returns
  `Undecided(Io(..))`.
- **Object storage.** The head object is the only mutable object and changes only by a
  conditional replace. An advance puts the entry as a group object named by its position
  and digest, create-if-absent, then replaces the head with one that names its claim, the
  new end and the entry's digest, conditional on the entity tag the writer's own previous
  replace returned. The writer takes that tag only from its own replace's response, or
  from a read of the head that shows its own claim at its own end, never from any other
  read. A head names its claim and end, so no two heads have equal bytes and no tag
  recurs; a replace whose response was lost is therefore retried as the identical
  request, of which at most one lands. A claim reads the head and replaces it
  conditionally, or creates it when absent; of racing claims one replace succeeds. Every
  entry names its predecessor's digest, so the head reaches every retained entry by name:
  recovery reads the head and gets each group by its name, back to what it already holds.
  It lists `h/` only as a hint that lets those gets run in parallel, and a group the
  listing omits is fetched by name, never taken as missing. Recovering `n` groups costs a
  head read, `ceil(n / 1000)` listing pages and `n` gets, or a head read and `n`
  sequential gets with no listing. Reclamation removes only group objects at positions up
  to the head's end that the chain does not name. An object-storage entry whose endpoint
  does not honour create-if-absent and conditional replace refuses every claim with
  `Unsupported(Conditional())`; `objects.s3@1` uses `If-None-Match: *` and `If-Match`.
- **Memory** keeps the history in the host process for tests and scratch stores.
- **Simulated.** `authority.sim@1` represents a log or object storage by the same rules
  and injects, by seed, what the store must survive by detecting and refusing, never by
  returning wrong data: the loss of anything not yet durable at a level, torn and garbage
  writes, refused requests, lost responses, failed barriers, racing claims, incomplete
  listings and virtual latency. Its decisions are recorded provider outcomes, it runs
  exactly the same engine, and it is the one fault path; the engine has no other fault
  seam.

**Cost per commit group.** The log: one `write` and, at `host`, one `fdatasync`,
plus the lock, unlock and length check, with no read. Object storage: two requests (the
segment put and the head replace); the next group's segment put may be in flight while
this group's head replace is pending, and head replaces are serialized. A claim costs a
read of the history to its end and one synchronized append, or two requests. Taking a
snapshot costs no request.

**History and derived objects.** The history is **authoritative**: only the head writes
it. Every other object (a checkpoint of the state at a version, an index part) is
**derived**: its bytes are a function of the history alone, in one canonical encoding,
with no writer identity. Any machine that reads the history may put a derived object,
and two that derive the same object write equal bytes. The history records the runtime
contract revision in force beside every code hash it names. Their bytes are declared
under **History format** below.

**Levels.** A provider declares its own ordered levels ([STORE-2](#store-2-transactions)),
names the failures each survives, and documents how. A provider that acknowledges at a
fast level and publishes to a stronger one later declares both, and each `D(L)` reports
its own progress. There is no level every provider must offer and no mapping between
providers' levels ([RUN-13](runners.md#run-13-provider-seams)); a deployment that moves a
place to another provider names that provider's levels in its grant and its callers'
runners. The grant's floor is a name the provider declares, checked before entry.

**Group commit.** Commits apply to the head's state at once, raising `V`, and queue. A
queue is advanced as one group when an outcome awaits its durability or a group bound in
the configuration is reached, never on a timer. **The head never holds a key across an
authority request:** ordering and running a transaction wait on no advance, no object
read and no other machine, so a contended key is served at memory speed whatever the
authority.

**Limits.** `key_bytes` 4096, `value_bytes` 1048576, `transaction_bytes` 16777216 and
`range_rows` 10000 by default. They are store state, fixed when the store is created and
recorded by its Genesis commit. A creation whose largest group the format could not frame
within `authority.max_entry` (**Bounds** below) refuses with `Unsupported(Size(..))`, so
no commit within the limits is undecidable for size. A refused limit has no effect and is
definite, never `Unknown(..)`: `store.open` checks the limits it is passed before it
claims, whether or not the store exists, because a claim is an effect. **An incomplete
creation is no store:** a history holding nothing but claim entries, as a failure between
the first claim and its durable Genesis leaves it, has acknowledged nothing, and the next
open creates the store on it, writing the Genesis after its own claim with the limits it
is passed. Only a history whose every entry is an intact claim is read so; damage to any
entry, the Genesis of an established store's included, is corruption. A deterministic bound on the work one body may do, equal
on every engine, is **Reserved**. It must charge every core operation by its operands, as
the [long pure primitives](determinism.md#run-configuration) are charged, because an
operation the host implements costs one step whatever its size; and its charges belong to
the runtime contract revision a call record names, because step counts follow how a
compiler lowers the code. Until it exists a body is bounded only by its run's memory
budget, which is not equal across engines and so decides no outcome: exceeding it stops
the run that holds the store, and every call that run had not released ends
`InDoubt(Abandoned())`.

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
magic at the start of every record: six bytes naming the record's family (`HSHIST`,
`HSNODE`, `HSHEAD`) and two version digits. A reader checks the magic before any digest
or sum. A magic of the record's family with other version digits refuses the open with
`Unsupported(Format(found, object))`, carrying the magic found; any other magic is
corruption. Either refusal removes nothing, cuts nothing and writes nothing. A format
change is a new magic and a cutover, never a second reader.

**Groups.** The history is a chain of **groups**, each the commits of one advance:

```text
group  = "HSHIST01" prev:[32] first:u64 epoch:u64 count:u32 commit* defs:u32 def*
def    = path:bytes fingerprint:bytes code:[32] revision:bytes
commit = kind:u8 body writes
writes = n:u32 { op:u8 key:bytes [value:bytes if op = 1] }*
```

`prev` is the **digest** `H(group)` of the previous group, all zeros for the first;
`first` is the commit version of its first commit (versions start at 1); `epoch` is the
writer epoch of the head that wrote it. The definition table names each distinct entry
the group's commits ran (its identity: definition path, signature fingerprint and code
hash, with the runtime contract revision, [RUN-8](runners.md#run-8-identity-and-arrival)) once, and commits refer to it by index.
The table follows the commits, so a writer encodes each commit once into the group's
buffer as it is ordered and appends the table and the count when it cuts the group.
A write is a put (`op` 1) or a delete (`op` 2); keys strictly ascend, so a key occurs
once. The SHA-256 of a commit's `writes` bytes is its write-set digest; it is derived,
never stored. A group is valid only if every field decodes, nothing follows its
definition table, it holds at least one commit, every index and op is in range, version
1 is a Claim, and a claim is alone in its group. Across groups, the first commit that is not
a claim is the Genesis, which occurs once: a history begins with one or more claim
groups, then the Genesis. It **follows** the previous group when `prev` is
that group's digest, `first` is one above its last version, and either it is a claim
group whose `epoch` is one above the previous group's and whose claim names the
history's identity (the first claim mints it), or it is not a claim group and has the
previous group's `epoch`. The digest chain is the history's
integrity: a reader that trusts a digest trusts every group before it.

Commit kinds:

| Kind | Body after `kind` | Records |
|---|---|---|
| 1 Transaction | `def:u32 at:i64 seed:u64 caller:bytes stream:u8 [identity:[32] position:u64 digest:[32] if stream = 1] args:bytes outcome:bytes` | The call record ([STORE-2](#store-2-transactions)): the entry, the call information ([STORE-4](#store-4-call-information)), the call stream, position and request digest it arrived with ([STORE-12](#store-12-transactions-at-a-runner)), the argument bytes and the canonical bytes of the `Result[R, TxFailure[E]]` it returned. |
| 2 Engine | `op:u8 payload:bytes` | A commit the engine makes on its own behalf, one of the operations below. An unknown `op` is corruption. |
| 3 Run | `run:[16] n:u32 { kind:u8 payload:bytes }*` | The journal records of a run at the place ([STORE-18](#store-18-runs-at-a-place)), with the kinds and payloads of the [durable journal](determinism.md#durable-journal). |

Every kind ends with its `writes`. Engine operations and their payloads:

| Op | Payload |
|---|---|
| 1 Genesis | The limits `key_bytes value_bytes transaction_bytes range_rows` and the checkpoint node bounds `node_min node_mean node_max`, each `u32`; the bounds must hold the limits (**Checkpoints** below). It is the first commit after the claims the history begins with (commit 2 unless a creation was cut before its Genesis), and nowhere else. |
| 2 Claim | `epoch:u64 attempt:[16] identity:[32]`: a claim, one above the previous epoch, with the claimant's random attempt identity and the history's identity. It is alone in its group, whose `epoch` is its own, and every later group carries that epoch until the next claim. Commit 1 is the first claim, which mints the identity; every later claim repeats it. |
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
The defaults are 256 KiB, 1 MiB and 4 MiB. Bounds **hold** limits when `0 < node_mean`,
`node_min <= node_mean <= node_max`, the largest leaf entry the limits allow fits a node
(`13 + 8 + key_bytes + value_bytes <= node_max`) and so do two of the largest index entries
(`13 + 2 * (44 + key_bytes) <= node_max`), so every level above the leaves has fewer
nodes than the one below; `node_max` is at most the backend's object cap. A root index
has at least two children, and a node is empty only as the root of an empty state. A **descriptor** is
`version:u64 digest:[32] root:[32] height:u8 entries:u64 bytes:u64`: the checkpoint's
version (always a group's last), the digest of that group, the root node, the number
of levels (1 for a lone leaf), the number of entries, and their key and value bytes. A checkpoint counts only once a Checkpoint commit after its version records its
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

**Object storage.** A group is the object `h/` followed by its history position as 20
decimal digits, `-` and its digest in lowercase hexadecimal, put only if free; then the
head is replaced conditionally on the tag of the writer's own last replace. A node is `n/` followed by its
hexadecimal digest. A group object no head record's chain reaches, left by a writer
fenced or undecided before its head replace, is not history and is reclaimed. A listing
only finds candidates: every object is checked against the digest that names it.

**Bounds.** `transaction_bytes` bounds one encoded commit, its call record and outcome
included, so whether a transaction fits depends only on its run. Creating a store whose
`transaction_bytes` plus 8 KiB of group and definition overhead exceeds
`authority.max_entry` refuses with `Unsupported(Size(..))`, and a group is cut before it would exceed that, so
every commit the limits admit fits in one advance. The frame's `u32` length bounds a
local group at 4 GiB.

**Recovery.** Opening reads, in order:

1. the head record, and refuses unless its sum, magic and identity hold;
2. the checkpoint it names, nodes level by level, every leaf in parallel, validated as
   above;
3. the groups after the checkpoint: on a local log from the locator by sequential reads;
   on object storage by following `prev` back by name from the group whose position and
   digest the head names to the checkpoint's digest, getting in parallel the candidates
   a listing of `h/` offers and the rest by name; other candidates are orphans;
4. each group in order, checking its chain fields and applying its write sets.

Only the last frame of the last local segment can be **torn**: an incomplete final
frame, which recovery cuts. Anything else that fails is **corruption**: a bad digest,
sum, chain field or decoding, a missing referenced object, or a log that ends below the
head record's `end`. Corruption refuses the open with
`Corrupt(..)` naming the object, and removes nothing. A crash may tear or lose a
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
([RUN-4](runners.md#run-4-calls)), through the provider `db@1`
([providers](runners.md#providers)):

```text
fn runner.transact[A: serializable, R: serializable, E: serializable](r: Runner, f: Entry[fn(mut tx: Tx, call: CallInfo, a: A) -> Result[R, E]], a: A) -> Result[R, RunError[TxFailure[E]]]
fn runner.query[A: serializable, R: serializable, E: serializable](r: Runner, f: Entry[fn(tx: Tx, call: CallInfo, a: A) -> Result[R, E]], a: A) -> Result[R, RunError[TxFailure[E]]]
```

`runner.transact` and `runner.query` name `Tx`, `CallInfo` and `TxFailure`, which
`std@1/store` declares, so they are declared beside them. A place serves only the entries
its head serves; no caller reaches a store's rows any other way.

#### Serving

```text
Served   // opaque: copyable, inert, sendable; one entry a head serves, with its form
fn store.served_transact[A: serializable, R: serializable, E: serializable](f: Entry[fn(mut tx: Tx, call: CallInfo, a: A) -> Result[R, E]]) -> Served
fn store.served_query[A: serializable, R: serializable, E: serializable](f: Entry[fn(tx: Tx, call: CallInfo, a: A) -> Result[R, E]]) -> Served
fn store.serve(mut s: Store, take inbox: Inbox, entries: List[Served]) -> Result[(), StoreError]
```

The **head run** of a place is the run that holds its `Store`. Its host routes to it the
transact and query calls that pass the host's arrival checks, through the `Inbox` its
`Io` projects ([`db@1`](runners.md#providers)), and the head serves them with
`store.serve` until the inbox closes, which returns `Ok(())`, or the store closes, which
returns the store's error. An `entries` list that names one path twice is
`InvalidInput()`, before any arrival is taken.

**Naming and resolution.** A served call names its entry only by identity: its path,
signature fingerprint and code hash ([RUN-3](runners.md#run-3-entries)), never by a
closure, a code address or a name the caller chose. The head resolves it against
`entries`, whose identities are those of the head run's own revision: a path it does not
serve, or serves in the other form, is `NotRun(Denied())`; a path it serves whose
fingerprint or code hash differs is `NotRun(VersionMismatch())`, reporting the head's
hashes as missing. A client's call names only path and fingerprint
([STORE-17](#store-17-clients)) and is resolved on those. The head never loads code to
serve a call. These are, for a place,
check 3 and the allowlist of check 4 of [RUN-8](runners.md#run-8-identity-and-arrival);
the host makes the other checks before the call reaches the head.

**Each arrival.** For each arrival, in the order it chooses, `store.serve`:

1. resolves its entry as above;
2. for a transact call, decides its stream request in the transaction that would run it
   (call streams, below); a position with a recorded outcome runs nothing;
3. decodes the argument bytes against `A` ([SER-2](serialization.md#ser-2-canonical-codec)),
   else `NotRun(Rejected())`;
4. mints the call information ([STORE-4](#store-4-call-information)) with the arrival's
   caller, and evaluates the entry ([STORE-15](#store-15-evaluation-and-re-execution)): a
   transaction on the head's state, committing as [STORE-2](#store-2-transactions) says,
   or a query on `store.snapshot(s, L)`;
5. ends the arrival with its **call end** once the gate releases it: when `D(L)` reaches
   its gate version, for `L` the stronger of the level the request names and the place's
   floor ([STORE-2](#store-2-transactions)).

`store.serve` waits on no backend request to order or evaluate an arrival; only each end
waits for its release. It promises no order between arrivals on different streams.
`db.local@1` evaluates them one at a time, in the order the inbox delivers them.

**Call ends.** A call's outcome is one value of the core type `CallEnd`
([frames](runners.md#providers)), the same on the wire, in a stream row and in a
recording; its canonical bytes are the call's outcome bytes
([STORE-2](#store-2-transactions)):

| The call | `store.transact`, `store.query` | `runner.transact`, `runner.query` | `CallEnd` | On the stream |
|---|---|---|---|---|
| The body returned `Ok(r)` | `Ok(r)` | `Ok(r)` | `Returned`, the bytes of `r` | recorded |
| The body returned `Error(e)` | `Error(Aborted(e))` | `Error(Failed(Aborted(e)))` | `Failed`, the bytes of the `TxFailure[E]` | recorded |
| The commit exceeded a limit `l` | `Error(Limit(l))` | `Error(Failed(Limit(l)))` | `Failed` | recorded by the stream rows alone |
| The body failed with `f` | `Error(Faulted(f))` | `Error(Faulted(f))` | `Faulted` | not recorded |
| The backend refused or failed the commit (`e`) | `Error(Storage(e))` | `Error(InDoubt(Abandoned()))` | `InDoubt` | not recorded |
| Durability could not be decided (`e`) | `Error(Unknown(e))` | `Error(InDoubt(Abandoned()))` | `InDoubt` | not recorded |
| Cancelled before the head took it | | `Error(Canceled(cause))` | `Canceled` | not recorded |
| Refused at arrival, resolution or the stream | | `Error(NotRun(..))` | `NotRun` | not recorded |

`TxFailure`'s `Canceled`, `Faulted` and `Unknown` never occur inside `Failed`: the runner
reports cancellation, failure and doubt itself ([RUN-6](runners.md#run-6-outcomes)). A
served call can be cancelled ([RUN-9](runners.md#run-9-cancellation)) only until the head
takes it from its inbox; from then on it ends with its own outcome.

**Recording.** A served call starts no callee run
([RUN-10](runners.md#run-10-callee-runs)): its body runs as an evaluation inside the head
run, and its record is its call record in the history ([STORE-10](#store-10-recording)),
whose stream, position and request digest link it to the caller's recorded request
([RUN-11](runners.md#run-11-linked-tapes)). The head run records each arrival it takes and
each end it gives as capability inputs and effects at its own level
([RUN-14](runners.md#run-14-recording-levels)), so it replays, bodies included, from its
tape. A durable caller journals a request before it is sent and its completion after the
end arrives; an end is released only at its level, so no completion a caller journals
depends on state that level could lose.

**Local calls.** `store.transact`, `store.query` and `store.call` evaluate as
`store.serve` does, with the entry the caller names from its own revision and an empty
caller ([STORE-4](#store-4-call-information)). The store's side of a transact call is one
request on a call stream, `CallRequest`, a root core data type
([frames](runners.md#providers)):

```text
CallFailure[E] = Tx(TxFailure[E]) | Denied() | Rejected()
fn store.call[A: serializable, R: serializable, E: serializable](mut s: Store, req: CallRequest, f: Entry[fn(mut tx: Tx, call: CallInfo, a: A) -> Result[R, E]], a: A) -> Result[R, CallFailure[E]]
```

`store.call` is `store.transact` as one request on a call stream, decided as the contract
below says. `Denied()` and `Rejected()` are the runner's `NotRun(Denied())` and
`NotRun(Rejected())`; `Tx(..)` is the transaction's own outcome.

- **Call streams.** Each caller sends its transact calls to a place on an ordered call
  stream, so the state a store keeps grows with the number of callers, not of calls. A
  stream belongs to one caller at one store: a caller run, with identity
  `H("hirsel.stream.v1", caller name, run instance)`, or a client instance
  ([STORE-17](#store-17-clients)); every stream identity is 32 bytes, and a request whose
  identity is not is `NotRun(Rejected())`. Under the reserved key `hirsel.streams/`
  followed by the identity, the store keeps the stream's **head row**: its `epoch`, its
  `applied` position and its `acknowledged` position. Under that key followed by a
  position as a big-endian `u64`, it keeps one **outcome row** for each position above
  `acknowledged` and at most `applied`: the request digest and the call end. A stream's
  identity and positions are logical: they do not change when the head moves to another
  machine or the store to another backend.
- A request carries the stream identity, the caller's epoch, a position, the request
  digest and the highest position whose outcome the caller has made durable. Positions
  start at 1 and are consecutive, and a caller has at most one request in flight per
  stream; a request that ends in `NotRun` or `Faulted` leaves its position to the caller's
  next request. The store decides each request in the transaction that would run it: an
  epoch below the head row's is `NotRun(Denied())`; a position at or below
  `acknowledged`, or above `applied + 1`, is `NotRun(Rejected())`; a position above
  `acknowledged` and at most `applied` returns its recorded end when the digest is equal,
  without decoding it at a served call, and is `NotRun(Rejected())` when it differs;
  position `applied + 1` runs the body, and the same commit puts its outcome row, sets
  `applied`, raises `acknowledged` and deletes the outcome rows at or below it. A request
  whose epoch exceeds the head row's raises it in the commit that decides the request, a
  duplicate's included. An end that commits no writes is recorded in a transaction of its
  own that writes only the stream rows. The stream rows are writes of the commit and count
  against the store's limits: when the body's writes and the rows recording their end
  exceed them, the end is `Limit(..)`, recorded by a commit of the stream rows alone; when
  not even those fit, the request is refused `Limit(..)` without being recorded and its
  position stays `applied + 1`.
- **Fencing.** A durable caller's executor holds an epoch for each stream, recorded in its
  journal header. A live continuation first commits, on each store the run used, a fence
  request that raises the stream's epoch and returns its `applied` position and retained
  ends. A `Prepared` position at or below `applied` then has its recorded end, and one
  above it proves the transaction never committed, so a transact is never in doubt. A
  caller ends its stream by acknowledging its last position; an administrator may purge
  abandoned streams, which the store records, and a purged stream's requests are
  unresolvable thereafter rather than "never run".
- A store that is fenced, whose backend fails, or whose durability cannot be decided ends
  every unreleased call with `InDoubt(Abandoned())`, which the caller's stream resolves.

**Across places.** Places exchange only runner calls. A transact call is exactly once at
its place; no call makes two places commit together. A library protocol over call
streams, in which each place first commits a hold on the keys it will change and a
coordinator then commits one decision, is **Reserved**. Its one requirement of the engine
is that the store, which chooses the order, runs a transaction that touches a held key
range only after the hold is decided. Serving leaves that open: the engine may evaluate
an arrival, find in its access set ([STORE-14](#store-14-tracked-access)) a held range,
discard the run unobserved and order the arrival again once the hold is decided. Its
stream position is consumed only by the commit that decides it, `store.serve` promises no
order across streams, and the wait is for another commit, never for a backend request.

### STORE-13: Failures

```text
TxFailure[E] = Aborted(E) | Canceled(CancelCause) | Faulted(Failure) | Storage(StoreError)
             | Limit(TxLimit) | Unknown(StoreError)
TxError      = NoTable() | TableExists() | Schema(SchemaError) | ReadOnly() | OutOfScope()
             | InvalidInput() | Limit(TxLimit) | Storage(StoreError)
SchemaError  = KeyMismatch() | ValueMismatch() | Renamed(stored: String, expected: String)
StoreError   = Storage(AuthorityError) | Refused(AuthorityError) | Corrupt(String) | Fenced()
             | Closed() | Unsupported(Unsupported) | NoLevel() | Foreign() | InvalidInput()
Unsupported  = Format(found: Bytes, object: String) | Size(needed: u64, max_entry: u64)
             | Conditional()
TxLimit      = KeyBytes() | ValueBytes() | TransactionBytes() | RangeRows()
```

Variant ordinals in the order written, `serializable` (`TxFailure[E]` when `E` is).
`TxFailure` is the outcome of a transaction: `Aborted(e)` when the body returned
`Error(e)`; `Canceled(cause)` before any run; `Faulted(f)` when the body failed with `f`
([FAIL-2](failure.md#fail-2-boundary)), with no effect; `Storage(..)` and `Limit(..)` when the
commit could not be made, with no effect; and `Unknown(..)` when the commit was attempted
and its durability could not be decided (STORE-11), which never claims an effect or its
absence. `TxError` is the outcome of one operation inside a body. `Storage(e)` is an
error from the authority, a full medium included; `Refused(e)` that the authority refused
a request before it had any effect; `Corrupt(name)` that the named part of the history or
object failed a checksum, decoding or structural check, or is missing; `Fenced()` that a
later writer claimed the store; `Closed()` that the store closed earlier. Each of these
closes the store. `Unsupported(..)` is definite and has no effect: `Format(found,
object)` that the named record is in another history format version, whose magic it
carries (an open, which refuses and removes nothing); `Size(needed, max_entry)` that a
creation's limits need groups larger than the authority's largest entry; `Conditional()`
that an object-storage endpoint does not honour create-if-absent and conditional
replace. `NoLevel()` (a level name the authority does not declare), `Foreign()` (a
ticket of another store) and `InvalidInput()` are definite, have no effect and leave the
store open.
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
nothing. It yields the outcome bytes, the write set, the access set
([STORE-14](#store-14-tracked-access)) and the watch set ([STORE-16](#store-16-subscriptions)). It is the engine's one way to run a body: a
commit is an evaluation at the head followed by an advance of the history, and a query is
the evaluation of a query entry. Local, served, replicated and re-executed calls evaluate
alike: the engine decodes the argument bytes ([STORE-2](#store-2-transactions)) and applies
the entry with `runner.apply_mut` to a `Tx` of its transaction, or with `runner.apply` to a
`Tx` of a query ([RUN-3](runners.md#run-3-entries)), so a body's failure is an outcome of
the evaluation and never escapes it. Whether a body exceeds a limit is part of its outcome
and the same in every engine; where an engine stops it is not, since its writes are
discarded ([STORE-11](#store-11-engine-and-storage-seam)). A place that evaluates a call
record it did not serve loads the code it names by its hash
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

A **subscription** is a query entry ([STORE-2](#store-2-transactions)), with its caller and
argument, kept current. Each evaluation of it ([STORE-15](#store-15-evaluation-and-re-execution))
registers two sets with the store:

- its **read set**, the reads of its access set ([STORE-14](#store-14-tracked-access)). A
  commit that writes into it makes the subscription **stale**: its query is evaluated again
  at that commit's version, and its new outcome, read set and watch set replace the old.
- its **watch set**, the key ranges the run watched. The writes of a commit that fall inside
  it are the **matched writes**, delivered as rows without evaluating anything.

```text
fn store.watch(tx: Tx, from: RangeBound[Bytes], to: RangeBound[Bytes]) -> Result[(), TxError]
fn table.watch[K: key + serializable, V: serializable](tx: Tx, t: Table[K, V], from: RangeBound[K], to: RangeBound[K]) -> Result[(), TxError]
```

`table.watch` adds to the run's watch set the rows of `t` whose keys lie between `from` and
`to` in key order ([STORE-6](#store-6-key-encoding)); `store.watch` adds the byte keys in that
range outside the reserved namespace. Each makes the checks of a read of the same range, in
the order of [STORE-5](#store-5-tables), scope ([STORE-3](#store-3-transaction-scope))
included, and fails as that read would. A watch reads no row: it adds nothing to the access
set, so a write inside a watched range never makes its subscription stale. The catalog reads
of `table.watch` are tracked like any table operation's, so dropping or migrating a watched
table does. A watch set is a set: watching a range twice watches it once. A watch outside a
subscription's evaluation, in a transaction or a plain query, has no effect after its checks.

A run grants a range by watching it: what a session receives as rows is exactly what the
exported entry's code watched, under the `Tx` it narrowed. One mechanism serves every live
read: query results through the read set, row replicas through the watch set. A maintained
view is a library that writes derived tables, in the transaction that changes their inputs
or after it; subscribers watch a derived table as they watch any other.

**The delivery rule.** A **session** is one subscriber's connection to a place
([STORE-17](#store-17-clients)), holding any number of subscriptions. The place sends it
**batches**, and what a session holds after applying one is observable to it as follows:

1. **One version.** Every batch is at one commit version, never below the previous batch's.
   After applying it the session holds, for every subscription, the outcome of its query at
   that version and the rows of its watch set at that version, and no other rows: all of a
   session's subscriptions advance together.
2. **Complete.** A batch holds every change that the commits after the previous batch's
   version, up to its own, caused the session. A version that changed nothing the session
   holds sends nothing, and a lagging session receives one batch for several versions. A
   batch at the version of the previous one carries only changes the session asked for: a
   subscription added, ended or dropped, or an outcome of its own call.
3. **Gated.** A batch at version `v` leaves the place only when `D(L) >= v` for the session's
   level `L` ([STORE-2](#store-2-transactions)).
4. **No gap.** A new subscription is evaluated at the session's frontier, the version up to
   which the matcher has compared commits with the session, and is registered before the
   next commit is compared, so no commit falls between the version its result was read at
   and the first commit it is matched against.
5. **Unchanged outcomes are suppressed.** A stale subscription whose new outcome bytes equal
   its last delivered ones delivers no outcome; a change of its watch set is still
   delivered.
6. **Only commits.** The passage of time never causes a batch; a query that depends on time
   receives it as an argument.

The matcher runs after a commit is ordered and never while the head holds a key
([STORE-11](#store-11-engine-and-storage-seam)); any machine that follows the history can
run it ([STORE-20](#store-20-one-history-many-machines)). Its costs are part of the engine
provider's contract, not of the rule:

- Registrations of equal entry, caller and argument bytes share one registration and one
  evaluation per version, since a query's call information depends only on its caller and
  the version it reads. A session no commit touches costs nothing per commit.
- Per commit, the matcher looks each written key up in an ordered index of registered keys
  and ranges, at a cost logarithmic in the registrations plus the matches. A stale
  registration costs one evaluation; a matched watch costs no evaluation and no decoding,
  since its rows go out as the stored bytes.

### STORE-17: Clients

A **client** is a program outside the place's trust: a browser or an application. It is an
untrusted, partial follower of the store: it calls the entries the place exports to
clients, holds the rows its subscriptions cover at one commit version, and resumes from that
version. A capability never reaches a client ([CAP-5](capabilities.md#cap-5-capability-sealing)).
**Authorization** is the code of the exported entries: an entry narrows its `Tx` to what its
caller may reach ([STORE-3](#store-3-transaction-scope)) before it calls anything else, and
its reads are tracked, so a subscription that depended on a membership row is stale when
that row changes and its watches are withdrawn in the batch that delivers the change. There
is no rule language, and no row is readable except through an exported entry.

#### Ingress

A place's host serves clients on a WebSocket ([RFC 6455](https://www.rfc-editor.org/rfc/rfc6455))
over TLS. The upgrade request names the subprotocol `hirsel.feed.` followed by the 64
lowercase hexadecimal digits of the SHA-256 of the concrete schema descriptor of `FeedFrame`
([SER-2](serialization.md#concrete-schema-descriptor)); a host whose digest differs selects
no subprotocol, which fails the handshake. A browser's `Origin` must be on the host's
`origins` list when that list is nonempty. Every WebSocket message is one binary message
holding one `FeedFrame` as its canonical codec payload, without an envelope: the
subprotocol fixed the schema once for the connection. A text message, a frame that does not
decode or one longer than `frame_bytes` closes the session with `Malformed()`.

The client speaks first, with `Hello`; the place answers `Welcome` or `Closed(..)`. The
ingress runs at the head or at any follower ([STORE-20](#store-20-one-history-many-machines)):
a follower forwards calls to the head and serves batches at the versions it has applied.

- **Token.** `token` is a JWS compact serialization of a JSON Web Token
  ([RFC 7519](https://www.rfc-editor.org/rfc/rfc7519)), sent in the frame, never in the URL
  or a cookie. It verifies under one configured issuer when its `alg` is on that issuer's
  list (`EdDSA` with Ed25519, `ES256`, or `RS256` with a key of at least 2048 bits; never
  `none`), its `kid` selects a key of that issuer's key set, its signature verifies, `iss`
  equals the issuer's `issuer`, `aud` contains its `audience`, `sub` is nonempty UTF-8 of at
  most 255 bytes, `exp` is present and in the future and `nbf`, when present, is not, each
  with the issuer's `leeway_ns` against the host's clock. Otherwise the session closes
  `Denied()`. A host with `anonymous: true` also admits an empty token.
- **Principal.** A verified token's principal is `user:`, the issuer's configured `name`
  (1 to 32 characters of `a`-`z`, `0`-`9` and `-`), `:`, and its `sub` verbatim; an empty
  token's is `anon:` followed by the 32 lowercase hexadecimal digits of `instance`, which
  authenticates nothing. The place passes the principal to the serving primitive as the
  caller of every call and subscription of the session, and it reaches the body as
  `call_info.caller` ([STORE-4](#store-4-call-information)). It is fixed for the session:
  `Reauth(token)` before `exp` extends the session when the new token verifies to the same
  principal and closes it `Denied()` otherwise; a session whose last token expires closes
  `Expired()`.
- **Surface.** The host's `entries` list the exported entries by full versioned path and its
  `tables` the tables a client may hold rows of. `Hello.entries` names each entry the client
  will use, with the signature fingerprint its binding was generated from
  ([RUN-8](runners.md#run-8-identity-and-arrival)), and `Hello.tables` each table, with the
  SHA-256 of the key and value storage descriptors ([STORE-7](#store-7-rows-and-storage-identity))
  its binding decodes. `Welcome` answers each with whether it resolved: an entry on the
  surface whose fingerprint equals the active revision's, a table on the surface whose
  current descriptors match. Calls and subscriptions then name an entry by its index in
  `Hello.entries`, and rows name a table by its index in `Hello.tables`. A client names a
  definition only by path and never selects code: every call runs the revision the place
  has active. A breaking change to an exported entry is a new entry
  ([STORE-19](#store-19-activation)); a watched table whose schema changed no longer
  resolves, and a subscription that watches it ends `Unavailable()`, never with rows the
  binding would misdecode.
- **Stream.** `instance` is 16 bytes the client draws once and keeps with its queue of
  pending calls. The session's call stream ([STORE-12](#store-12-transactions-at-a-runner))
  has identity `H("hirsel.client.stream.v1", principal, instance)`, so another principal
  cannot use it. `epoch` is the client's epoch for the stream: a `Hello` with a higher one
  closes the stream's earlier session `Superseded()`, and one below the stream's closes
  `Superseded()` itself. `Welcome.applied` is the stream's `applied` position.

`FeedFrame` and the types it reaches are declared by `std@1/store`, `serializable`, with
variant ordinals in the order written:

```text
FeedFrame        = Hello(FeedHello) | Call(FeedCall) | Subscribe(FeedSubscribe) | Unsubscribe(u32)
                 | Reauth(Bytes) | Welcome(FeedWelcome) | Batch(FeedBatch) | Closed(FeedClose)
FeedHello        = { token: Bytes, instance: Inline[u8, 16], epoch: u64, acknowledged: u64,
                     entries: List[EntryRef], tables: List[TableRef], resume: Option[FeedResume] }
EntryRef         = { path: String, fingerprint: Bytes }
TableRef         = { name: String, key: Inline[u8, 32], value: Inline[u8, 32] }
FeedResume       = { at: Inline[u8, 16], held: List[HeldSubscription] }
HeldSubscription = { id: u32, entry: u32, argument: Bytes, outcome: Inline[u8, 32], watches: List[FeedWatch] }
FeedWatch        = { id: u32, source: WatchSource, from: RangeBound[Bytes], to: RangeBound[Bytes] }
WatchSource      = Raw() | Table(u32)
FeedCall         = { position: u64, entry: u32, argument: Bytes, acknowledged: u64 }
FeedSubscribe    = { id: u32, entry: u32, argument: Bytes }
FeedWelcome      = { principal: String, applied: u64, entries: List[bool], tables: List[bool] }
FeedBatch        = { at: Inline[u8, 16], reset: bool, more: bool, items: List[BatchItem] }
BatchItem        = Value(u32, Bytes) | Ended(u32, SubscriptionEnd) | Dropped(u32)
                 | Covered(u32, FeedWatch, List[(Bytes, Bytes)]) | Uncovered(u32)
                 | Rows(WatchSource, List[(Bytes, Bytes)], List[Bytes]) | Decided(u64, CallEnd)
SubscriptionEnd  = Unavailable() | Rejected() | Overloaded() | Limit(TxLimit)
CallEnd          = Ok(Bytes) | Aborted(Bytes) | Limit(TxLimit) | NotRun(NotRunReason)
FeedClose        = Denied() | Expired() | Superseded() | Overloaded() | Malformed() | Stopped()
```

The first five arms travel from client to place, the last three from place to client; an arm
in the wrong direction is `Malformed()`. Argument and outcome bytes are canonical codec
payloads of the entry's parameter and result types. A row's key is the key encoding of the
table's `K` ([STORE-6](#store-6-key-encoding)), or the raw key, and its value is the
canonical payload of `V` without the stored schema epoch, or the raw value; bounds are in
the same encoding, so a client compares keys and coverage bytewise. A client never sees a
reserved key or how a library lays out its rows.

#### Calls

`Call` runs a transaction entry on the session's stream as
[STORE-12](#store-12-transactions-at-a-runner) decides it, with request digest
`H("hirsel.client.call.v1", entry path, argument)` and the client's `acknowledged`
position. The place accepts positions up to `applied + window` and runs them in order, so a
client needs no round trip per call; a position past that is `NotRun(Overloaded())`. An
index that did not resolve is `NotRun(VersionMismatch())`, a query entry is
`NotRun(Rejected())`, and an argument that does not decode is `NotRun(Rejected())`. Every
decision reaches the client as `Decided(position, end)`: a decision that commits rides in
the batch at its commit version, with every change that commit made to what the session
holds, so a client sees its own write and its outcome together; a refusal or a recorded
outcome returned again rides in the next batch. `Ok` and `Aborted` hold the payloads of `R`
and `E`. A client queues calls while disconnected and sends them again after `Welcome`: a
position at or below `applied` returns its recorded outcome, so a call is decided once
([STORE-12](#store-12-transactions-at-a-runner)). There is no cancellation of a sent call.
A place may purge a client stream idle for `stream_idle_ns` only when every decided position
is acknowledged; a client whose `Welcome.applied` is below its own acknowledged position
renumbers its pending calls from `applied + 1`, which is safe because none of them was
decided. A store that closes, is fenced or cannot decide a commit closes every session
`Stopped()`; the client reconnects, and its stream resolves every call it sent.

#### Subscriptions and batches

`Subscribe(id, entry, argument)` starts a subscription of a query entry, evaluated with the
session's principal as caller ([STORE-16](#store-16-subscriptions)); its first batch carries
its `Value` and a `Covered` for each watch, and until then the client shows it as loading.
`Unsubscribe(id)` ends it at the next batch, which carries `Dropped(id)`; the client holds
its state until then, and an `id` is not reused before its `Dropped`. A subscription ends
with `Ended(id, reason)` when its entry did not resolve or is not a query entry, or it
watches a table that did not resolve (`Unavailable()`); its argument does not decode
(`Rejected()`); the session is at `subscriptions` or the place lacks capacity
(`Overloaded()`); or an evaluation exceeds a store limit, the session's `watches`, or a
cover's `cover_rows` (`Limit(..)`). A query whose body returns `Error(e)` is not ended: its
`Value` holds the `Error` payload and it stays current.

A batch's items, in any order, mean:

- `Value(id, bytes)`: the subscription's new outcome, the payload of its `Result[R, E]`.
- `Covered(id, watch, rows)`: the subscription now watches `watch`, and `rows` are every row
  in its range at the batch's version, in key order; they replace what the client held in
  that range. A cover too long for one frame continues in further `Covered` items of the
  same watch in the same batch, whose rows together are the range. `watch.id` is the
  place's, unique among the session's watches; a resumed session keeps the ids of the
  watches it kept.
- `Uncovered(watch)`: that watch ended.
- `Rows(source, puts, deletes)`: matched writes of one table or of raw keys, each key at most
  once, last write winning, only inside watches the session holds both before and after the
  batch.
- `Decided`, `Ended`, `Dropped` as above.

A client applies a batch whole, then discards every row no remaining watch covers. A batch
too long for one frame is sent as consecutive frames at the same `at`, every one but the
last with `more` set; the client applies them together at the last. `reset` tells the
client to drop everything it holds for the session before applying. A held row is therefore
always the row at the version of the last batch, and a key inside a held watch with no row
is absent at that version; a key outside every held watch is unknown, never absent.

`at` names the batch's version to this place alone: it is the AES-128 encryption under the
host's `resume_key` of the commit version as a little-endian `u64` followed by the first 8
bytes of `H("hirsel.feed.at.v1", store identity)`, so it reveals no count of commits
([STORE-8](#store-8-versions-snapshots-and-forks)) and a token from another store or key
does not decrypt to this one.

**Resume.** A client that reconnects sends `resume` with the `at` of its last applied batch
and each subscription it still wants: its entry and argument, the SHA-256 of its held outcome
bytes and its held watches. Subscriptions it unsubscribed it drops itself. The place
evaluates each at a frontier `e` no lower than the resumed version, and answers with one
batch at `e`:

- a `Value` for each subscription whose outcome digest differs;
- for each watch it now holds: when the client claimed the same range, either the matched
  writes of the commits after the resumed version, read from the history, or a `Covered`
  that replaces the range, whichever the place finds cheaper; otherwise a `Covered`;
- an `Uncovered` for each claimed watch it no longer holds;
- a `Decided` for every retained outcome of the stream above `Hello.acknowledged`.

When `at` does not decrypt to a version of this store, or the history after it is not
retained ([STORE-8](#store-8-versions-snapshots-and-forks)), the batch has `reset` set and
covers everything again. The place keeps no state for a client between sessions beyond its
stream row.

**Prediction.** A client may evaluate a transaction entry on the rows it holds
([STORE-15](#store-15-evaluation-and-re-execution)) and show the result as provisional. It
sends the call, never its writes; the batch that carries the call's `Decided` also carries
its effects and replaces the provisional result. A read the client's rows do not cover makes
the prediction unavailable, never a guess. Nothing reaches a client before the output gate
releases it, and nothing about a call is decided at the client.

#### Bounds

A session costs the place its registrations and at most `send_bytes` of encoded batches it
has not yet written; the place never holds a client's rows. When the socket does not drain
and the next batch would exceed `send_bytes`, the session **lags**: the place stops building
batches for it and keeps only which subscriptions are stale and which watched keys were
written, up to `dirty_keys` of them. When the socket drains, one batch at the session's
frontier carries all of it, the rows read from the state at that version; past `dirty_keys`
it covers every held watch again instead. The writer never waits for a client, and a slow
client costs memory bounded by its registrations and these two settings.

The host's `clients` configuration holds the ingress settings; the bounds shown are the
defaults:

```json
{"listen": {"tcp": {"address": "0.0.0.0", "service": "443"}},
 "identity": {"chain": "/etc/hirsel/host.pem", "key": "/etc/hirsel/host.key"},
 "path": "/feed", "origins": ["https://app.example"],
 "issuers": [{"name": "auth", "issuer": "https://auth.example/", "audience": "app",
              "keys": {"jwks": "/etc/hirsel/auth-jwks.json"},
              "algorithms": ["EdDSA", "ES256"], "leeway_ns": 60000000000}],
 "anonymous": false,
 "entries": ["chat@1/threads.list", "chat@1/threads.send"], "tables": ["msg", "thread"],
 "level": null, "resume_key": "/etc/hirsel/feed.key",
 "sessions": 65536, "sessions_per_principal": 16, "subscriptions": 256, "watches": 1024,
 "cover_rows": 10000, "window": 16, "send_bytes": 4194304, "dirty_keys": 65536,
 "frame_bytes": 2097152, "stream_idle_ns": 2592000000000000}
```

`level` names a level the store's authority declares, `null` meaning the deployment floor;
a name it does not declare, or a level below the floor, is refused before the host serves. `resume_key` is a file of 16 bytes; replacing it makes every
resume a reset. `frame_bytes` must exceed the store's `key_bytes` plus `value_bytes` plus
64, so one row always fits a frame. A `Hello` past `sessions` or `sessions_per_principal`
closes `Overloaded()`.

#### Bindings

The bindings generator reads a program and its host's `clients` surface and emits one
TypeScript module. It contains:

- for each entry on the surface: its path and signature fingerprint, whether it is a
  transaction or a query, the types of `A`, `R` and `E`, an encoder of `A` and a decoder of
  `Result[R, E]`; a transaction entry is a function returning the call's `CallEnd`, a query
  entry a subscription delivering `Value`s;
- for each table on the surface: its name, the two descriptor digests, the types of `K` and
  `V`, an encoder and a decoder of `K` in the key encoding, and a decoder of `V`;
- the `FeedFrame` codec and the subprotocol name.

Types map structurally: `bool` to `boolean`; integers of at most 32 bits, `f32` and `f64`
to `number`; 64- and 128-bit integers to `bigint`, never `number`; `String` and `char` to
`string`; `Bytes` to `Uint8Array`; records to readonly object types with their field names;
tuples to readonly tuples; `List` and `Array` to readonly arrays; `Map` to a readonly array
of key and value pairs in its order; `Set` to a readonly array in canonical order; and every
variant, `Option` and `Result` included, to a union of objects tagged by `tag` with the arm
name, positional fields named `_0`, `_1` and so on; unit to `null`; `Inline` and `Simd` to
readonly arrays; and `box T`, `Packed[T]` and transparent nominal and distinct types to
their payload's type. Encoders and decoders are generated
straight-line code, not interpreters of descriptors, and decoders make every check of
[SER-2](serialization.md#validation) and [STORE-6](#store-6-key-encoding); a frame that
fails one closes the session from the client's side.

One runtime package serves every binding: the session with its reconnects, resume and
`Reauth`; the pending call queue, kept durably with `instance`, epoch and positions; and the
row cache, which keeps each table's rows ordered by their key bytes, so a range read in key
order needs no comparator, and answers a lookup as a row, absent or not held. It applies
each batch whole and notifies once per batch.

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

A head serves one revision, its own, and loads no code
([STORE-12](#store-12-transactions-at-a-runner)). The activation is the old head's last
commit. A head built from the new revision then claims the history, which fences the old
head ([STORE-11](#store-11-engine-and-storage-seam)): a fenced head stops serving, ends
every call it holds `InDoubt(Abandoned())` and leaves the calls it never took to the new
head. Callers retry through their call streams, which resolve each in-doubt position at
the new head exactly as at the old.

### STORE-20: One history, many machines

No result of any program depends on which machine holds a store's head, on how many
machines follow its history, on where its objects are kept, or on which authority keeps
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
