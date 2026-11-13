# Budgets

### MEM-19: Limits

A **limit** is a number of bytes with a running total of bytes used. The run's root limit
is granted to `main` by the entry ([PROG-3](programs.md#prog-3-entry)). Every other limit is
**reserved** from a parent limit: a bound scope's from the limit current where it is
entered (MEM-25), a task's from its spawner's current limit at spawn
([CONC-3](tasks.md#conc-3-spawn)), a finalizer grant's with its resource (MEM-27). At each
point of a program exactly one limit is **current**: the innermost enclosing bound scope's
or running finalizer's grant, or else the task's. Nothing can name a limit, and there is no
other budget object in v1.

Charges cover every block, node, region allocation, queue, task record, cause cell and
grant a program makes. They do not cover the call stack, host memory behind provider
operations ([CAP-5](capabilities.md#cap-5-provider-operations)), or the engine's own
bookkeeping: drop flags and the limit records of MEM-20.

### MEM-20: Limit records and hand-over

Every block, counted node, region allocation, queue, task record and cause cell is charged
to the limit current where it is made, except that growth of a collection with storage is
charged where MEM-26 and MEM-27 say, and refers to the record of the limit it was charged
to. A block keeps
its record when its owner is moved, sent, returned or received: its refund goes to the
limit the record leads to.

When a limit ends (its bound scope exits, its finalizer returns, or its task finishes), the
unused part of its reservation is refunded to its parent at once, and its record is joined
to the parent's: from then on the record leads to the parent, for refunds and for growth
(MEM-26), and if the parent ends in turn, on to its parent, up to the root. This
**hand-over** visits no value, admits nothing and cannot fail: the parent already holds the
surviving bytes as part of the reservation it gave. It covers everything the limit funded
that outlives it: a bound scope's result and anything moved outward, the cause cells of a
failure raised under it ([FAIL-10](failure.md#fail-10-cause-chain)), values in a queue, and a
task's result. So the lexical end of a limit and the death of what it funded are separate,
and the limit a block's record leads to is always live. Transferring or refunding across
tasks is an event in the run's event order ([CONC-10](tasks.md#conc-10-event-order)).

Records form a union-find forest, joined by rank with path compression; each tree's
representative points to the live limit it leads to, so a refund costs amortised
inverse-Ackermann time in the number of records. A record is freed once its limit has ended
and no block, reservation or record refers to it. Records are engine bookkeeping and are
not charged.

```hirsel
fn make(n: u64) -> Result[List[u64], Capacity] {
  let xs = bound bytes 1024 { list.with_capacity(n)? }?  // after the scope, xs is the caller's
  Ok(xs)
}
```

### MEM-21: Admission

Every operation that allocates admits its charge before any write: `used + n ≤ limit`
gives `used += n`; otherwise the operation fails with `Capacity` and nothing has been
allocated ([T5.6](ownership.md#own-26-the-transition-table)). For an allocating operation,
admission is its commit point ([FAIL-3](failure.md#fail-3-commit-points)). Every core
operation that may allocate says so in its result type, `Result[_, Capacity]` or an error
type that holds `Capacity` ([core](core.md)), with two exceptions: a call through a callable
value may allocate whatever the callee does, and an owning closure's environment is
admitted by its literal ([LANG-14](language.md#lang-14-captures)). A user function may
allocate and handle the `Capacity` itself, so a result type without `Capacity` does not
show that a function allocates nothing.

```hirsel
fn grow(mut xs: List[u64]) -> Result[(), (Capacity, u64)] { list.push(mut xs, 1) }
```
```hirsel
fn grow(mut xs: List[u64]) { list.push(mut xs, 1) }         // refused: LANG-18, unused Result
```

### MEM-22: The Capacity value

`Capacity` is a plain record `{ requested: u64, available: u64 }`: the charge that was
refused, and the room left in the limit the admission was tried against. Charges are
computed in exact arithmetic; one that exceeds `2^64 − 1` is refused with `requested`
`2^64 − 1`. Constructing, copying and returning a `Capacity` admits nothing. Two runs of the
same program with the same inputs and limits produce the same `Capacity` values at the
same points ([SEM-8](semantics.md#sem-8-determinism)).

### MEM-23: Refund

A block is refunded to the limit its record leads to (MEM-20) when it is freed: when its
owner is cleaned, when a region closes, when a counted node's last holder goes, when a
collection's storage is replaced by growth, and when a resource is wholly destructured (its
field storage, unused cause cell and unused grant,
[T3.7](ownership.md#own-26-the-transition-table)). `finish_trim`
refunds the unused end of a builder's allocation in the same way, and the allocation keeps
charging the rest ([T1.9](ownership.md#own-26-the-transition-table)). Refunding
cannot fail and admits nothing ([T5.7](ownership.md#own-26-the-transition-table)).
Mechanical release, region close and counted release never allocate
([FAIL-13](failure.md#fail-13-mechanical-release)); a user finalizer may, and admits only from
its grant (MEM-27).

### MEM-24: Logical charges

A charge is the logical size of what is allocated under the policy `charge-v1`:

| allocation | charge |
|---|---|
| `Box[T]` node | the payload's size, rounded up to 8 |
| `List[T]`, `String` storage | capacity × element size (`String`: 1), rounded up to 8 |
| `Map[K, V]` storage | capacity × (key size + value size + 16), rounded up to 8 |
| counted node | payload size + 8, rounded up to 8 |
| owning closure environment | the sum of its captures' sizes, rounded up to 8 |
| region allocation | its byte count, rounded up to 8 |
| a `Text` or `FixedSpan` made outside a compact type | 8 (count) + payload bytes, rounded up to 8 |
| a span builder's allocation | a region allocation of its current capacity × element size + 8 |
| a `build_in` or `build_stream_in` allocation | `n`, rounded up to 8; after `finish_trim`, its used bytes, rounded up to 8 |
| `seq.index_in` | 24 + count × 8, rounded up to 8 |
| resource value | one block: its fields' sizes, plus 32 for its cause cell, plus its grant (MEM-27) |
| task | 256 + its result's size + 32 for its primary cause cell + its reserved limit `n` |
| `main`'s task at startup | 256 + 32, from the root limit before `main` runs; a root limit smaller than this does not start the run |
| channel | capacity × item size + 64 |

`list.new`, `string.new`, `map.new`, `list.with_capacity(0)` and an empty span builder
allocate nothing and charge 0; an explicit `span.with_capacity(r, 0)` and an empty
`text.copy_in` still admit their count header ([core](core.md)). A `clone` or `clone_in`
charges each copy at capacity equal to its length, and a compact `clone_in` charges the
selected value's bytes when its type is streamable and the whole copied blob otherwise ([MEM-17](memory.md#mem-17-clone)). An allocation not in this table does not exist in v1.

Creating a resource admits its cause cell, and can therefore fail with `Capacity`; then
nothing was admitted, so nothing is refunded, and the literal's fields come back in its
error ([T1.2](ownership.md#own-26-the-transition-table)). This holds unless
it is **mechanical**: its cleanup can never add a cause. Exactly four resources are
mechanical, and they have no cause cell: `Sender`, `Receiver`, `Task` and `HostFile`. The
first three run no finalizer; their cleanup closes a channel or ends a child (CONC-8), and
a cause raised while it drops queued items or a result goes into the cell of the value whose
finalizer raised it. `HostFile`'s finalizer closes the host file and ignores the outcome
([core](core.md)). Every other resource, and every capability that is a
resource, admits its cell; `channel.sender` admits nothing.

Sizes are logical, never an engine's: scalars are their widths, `bool` 1, `char` 4, `()` 0; a
record or tuple is the sum of its fields; a variant type, plain or own, is 8 plus its largest
payload; a value of a declared resource type counts 8 wherever it is held, its fields, cell
and grant being charged once on its own block; a plain `fn` counts 8, and an `own fn`, a
`scoped fn` and a reference to a stable value count 16; a compact value held inline counts
its encoding. Every built-in type's size is in [core](core.md#built-in-types). Words an
engine adds of its own, such as alignment or an environment's drop recipe, are never
charged.

A collection grows to capacity `max(needed, 4, 2 × old capacity)`; the new storage is
admitted before the old is refunded, so growth briefly holds both. Nothing shrinks
storage automatically; `with_capacity(n)` admits exactly `n` elements. No allocator
constant, alignment or chunk size of an engine enters a charge. A later policy has a new
name; a package states the policy it was written for
([PROG-1](programs.md#prog-1-packages)).

### MEM-25: The bound scope

`bound bytes n { body }` evaluates to `Result[T, Capacity]` where `T` is the body's type.
Entering it admits `n` against the current limit and makes a new limit of `n` current for
the body; if the admission fails the body does not run and the result is `Error(c)`
([T5.4](ownership.md#own-26-the-transition-table)). It covers every allocation kind made in
the body: own, counted, region and task storage. In the body, `?` applied to an `Error(c)`
whose type is exactly `Capacity` exits the bound scope with `Error(c)`; `?` on any other
error type exits the function as usual. No exit crosses a closure body. On exit by any
edge, the body's views end and its owners are cleaned, the unused part of `n` is refunded
to the parent, and the record is handed over (MEM-20). `bound` is the only bounding form;
a region is not one.

```hirsel
fn copy_bounded(src: String) -> Result[Result[String, Capacity], Capacity] {
  bound bytes 65536 { string.from(src) }
}
```
```hirsel
fn f() -> Result[u64, Capacity] { bound bytes { 1 } }       // refused: LANG-5, bound needs a size
```

### MEM-26: Growth keeps the limit

A collection with no storage, empty or static, refers to no limit; its first growth is
charged to the current limit. Outside a finalizer (MEM-27), growing a collection that has storage charges the new storage to the limit
its current storage's record leads to, not to the current one, and refunds the old storage
there. So growing an outer owner that has storage inside a bound scope never touches the scope's limit:
whether the scope is exhausted makes no difference, and the growth is admitted or returns
`Capacity` against the owner's limit, which has already lost the scope's reservation `n`
(MEM-25).

### MEM-27: Finalizer grants

A resource type whose finalizer allocates, creates resources or spawns declares
`grant bytes n`; one without a declaration has a grant of 0. Creating a value of the type
reserves the grant as part of its charge (MEM-24). The grant is a limit of `n` bytes, and it
is the current limit while the finalizer runs: its allocations, and the storage, cause cell
and grant of every resource or task it creates, are admitted from it. A collection that grows inside a finalizer is admitted from the grant
too, whatever limit its storage refers to, and its old storage is refunded to the limit its
record leads to. Exceeding it is a
finalizer failure with cause `Capacity` ([FAIL-12](failure.md#fail-12-finalizer-failure)),
recorded in the resource's own cause cell ([FAIL-10](failure.md#fail-10-cause-chain)), and
abandons the finalizer body. When the finalizer ends, the unused part of the grant is
refunded, and what survives it (cause cells, sent values) is handed over to the limit that
funded the resource (MEM-20). So cleanup, failure cleanup included, never admits from any
limit but a grant.

```hirsel
resource type Log grant bytes 256 { sink: Sender[String] }
```

### MEM-28: Optimisation keeps charges

No optimisation changes which operations admit, how much they charge, when they refund or
which of them fail with `Capacity`. An engine that allocates less still charges the
logical amount; an engine that elides a copy still charges for it.
