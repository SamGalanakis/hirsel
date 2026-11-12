# Budgets

### MEM-21: Limits

A **limit** is a number of bytes with a running total of bytes used. The run's root limit
is granted to `main` by the entry ([PROG-3](programs.md#prog-3-entry)). Every other limit is
**reserved** from a parent limit: a bound scope's from the limit current where it is
entered (MEM-27), a task's from its spawner's current limit at spawn
([CONC-3](tasks.md#conc-3-spawn)), a finalizer grant's with its resource (MEM-29). At each
point of a program exactly one limit is **current**: the innermost enclosing bound scope's
or running finalizer's grant, or else the task's. Nothing can name a limit, and there is no
other budget object in v1.

Charges cover every block, node, region allocation, queue, task record, cause cell and
grant a program makes. They do not cover the call stack, host memory behind provider
operations ([CAP-5](capabilities.md#cap-5-provider-operations)), or the engine's own
bookkeeping: drop flags and the limit records of MEM-22.

### MEM-22: Limit records and hand-over

Every block, counted node, region allocation, queue, task record and cause cell is charged
to the limit current where it is made, and refers to that limit's **record**. A block keeps
its record when its owner is moved, sent, returned or received: its refund goes to the
limit the record leads to.

When a limit ends (its bound scope exits, its finalizer returns, or its task finishes), the
unused part of its reservation is refunded to its parent at once, and its record is joined
to the parent's: from then on the record leads to the parent, for refunds and for growth
(MEM-28), and if the parent ends in turn, on to its parent, up to the root. This
**hand-over** visits no value, admits nothing and cannot fail: the parent already holds the
surviving bytes as part of the reservation it gave. It covers everything the limit funded
that outlives it: a bound scope's result and anything moved outward, the cause cells of a
failure raised under it ([FAIL-10](failure.md#fail-10-cause-chain)), values in a queue, and a
task's result. So the lexical end of a limit and the death of what it funded are separate,
and the limit a block's record leads to is always live. Transferring or refunding across
tasks is an event in the run's event order ([CONC-10](tasks.md#conc-10-event-order)).

Records form a union-find forest, joined by rank with path compression; each tree's
representative points to the live limit it leads to. A refund therefore costs amortised
inverse-Ackermann time in the number of records, which is constant for every practical
run. A record is freed once its limit has ended and no block, reservation or record refers
to it, so live records are bounded by the live limits plus the ended limits that still
fund a block. Records are engine bookkeeping and are not charged.

```hirsel
fn make(n: u64) -> Result[List[u64], Capacity] {
  let xs = bound bytes 1024 { list.with_capacity(n)? }?  // after the scope, xs is the caller's
  Ok(xs)
}
```

### MEM-23: Admission

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

### MEM-24: The Capacity value

`Capacity` is a plain record `{ requested: u64, available: u64 }`: the charge that was
refused, and the room left in the limit the admission was tried against. Charges are
computed in exact arithmetic; one that exceeds `2^64 − 1` is refused with `requested`
`2^64 − 1`. Constructing, copying and returning a `Capacity` admits nothing. Two runs of the
same program with the same inputs and limits produce the same `Capacity` values at the
same points ([SEM-8](semantics.md#sem-8-determinism)).

### MEM-25: Refund

A block is refunded to the limit its record leads to (MEM-22) when it is freed: when its
owner is cleaned, when a region closes, when a counted node's last holder goes, when a
collection's storage is replaced by growth, and when a resource is wholly destructured (its
unused cause cell and grant, [T3.7](ownership.md#own-26-the-transition-table)). Refunding
cannot fail and admits nothing ([T5.7](ownership.md#own-26-the-transition-table)).
Cleaning, closing and counted release never allocate
([FAIL-13](failure.md#fail-13-mechanical-release)).

### MEM-26: Logical charges

A charge is the logical size of what is allocated under the policy `charge-v1`:

| allocation | charge |
|---|---|
| `Box[T]` node | the payload's size, rounded up to 8 |
| `List[T]`, `String` storage | capacity × element size (`String`: 1), rounded up to 8 |
| `Map[K, V]` storage | capacity × (key size + value size + 16), rounded up to 8 |
| counted node | payload size + 8, rounded up to 8 |
| owning closure environment | the sum of its captures' sizes, rounded up to 8 |
| region allocation | its byte count, rounded up to 8 |
| a `Text` or span made outside a compact type | 8 (count) + 8 (extent) + 8 per element for `IndexedSpan` + payload bytes, rounded up to 8 |
| resource value | its storage, plus 32 for its cause cell, plus its grant (MEM-29) |
| task | 256 + its result's size + 32 for its primary cause cell + its reserved limit `n` |
| channel | capacity × item size + 64 |

`Sender`, `Receiver` and `Task` have no cause cell: their cleanup cannot add a cause, and
`channel.sender` admits nothing.

Sizes are logical, never an engine's: scalars are their widths, `bool` 1, `char` 4, `()` 0; a
record or tuple is the sum of its fields; an own variant type is 8 plus its largest
payload; `Box`, `Rc`, `Arc`, a task handle, a `Child`, a capability and a plain `fn` count 8;
a `View`, an `own fn`, a `scoped fn` and a reference to a stable value (`Text` and spans
included) count 16; `List`, `String` and `Map` headers, `Sender` and `Receiver` count 24; a
compact value held inline counts its encoding.

A collection grows to capacity `max(needed, 4, 2 × old capacity)`; the new storage is
admitted before the old is refunded, so growth briefly holds both. Nothing shrinks
storage automatically; `with_capacity(n)` admits exactly `n` elements. No allocator
constant, alignment or chunk size of an engine enters a charge. A later policy has a new
name; a package states the policy it was written for
([PROG-1](programs.md#prog-1-packages)).

### MEM-27: The bound scope

`bound bytes n { body }` evaluates to `Result[T, Capacity]` where `T` is the body's type.
Entering it admits `n` against the current limit and makes a new limit of `n` current for
the body; if the admission fails the body does not run and the result is `Error(c)`
([T5.4](ownership.md#own-26-the-transition-table)). It covers every allocation kind made in
the body: own, counted, region and task storage. In the body, `?` applied to an `Error(c)`
whose type is exactly `Capacity` exits the bound scope with `Error(c)`; `?` on any other
error type exits the function as usual. No exit crosses a closure body. On exit by any
edge, the body's views end and its owners are cleaned, the unused part of `n` is refunded
to the parent, and the record is handed over (MEM-22). `bound` is the only bounding form;
a region is not one.

```hirsel
fn parse_bounded(src: String) -> Result[Result[Ast, ParseError], Capacity] {
  bound bytes 65536 { parse(src) }
}
```
```hirsel
fn f() -> Result[u64, Capacity] { bound bytes { 1 } }       // refused: LANG-5, bound needs a size
```

### MEM-28: Growth keeps the limit

A collection with no storage refers to no limit; its first growth is charged to the
current limit. Growing a collection that has storage charges the new storage to the limit
its current storage's record leads to, not to the current one, and refunds the old storage
there. So growing an outer owner inside a bound scope never touches the scope's limit:
whether the scope is exhausted makes no difference, and the growth is admitted or returns
`Capacity` against the owner's limit, which has already lost the scope's reservation `n`
(MEM-27).

### MEM-29: Finalizer grants

A resource type whose finalizer allocates, creates resources or spawns declares
`grant bytes n`; one without a declaration has a grant of 0. Creating a value of the type
reserves the grant as part of its charge (MEM-26). The grant is a limit of `n` bytes, and it
is the current limit while the finalizer runs: its allocations, and the storage, cause cell
and grant of every resource or task it creates, are admitted from it. Exceeding it is a
finalizer failure with cause `Capacity` ([FAIL-12](failure.md#fail-12-finalizer-failure)),
recorded in the resource's own cause cell ([FAIL-10](failure.md#fail-10-cause-chain)), and
abandons the finalizer body. When the finalizer ends, the unused part of the grant is
refunded, and what survives it (cause cells, sent values) is handed over to the limit that
funded the resource (MEM-22). So cleanup, failure cleanup included, never admits from any
limit but a grant.

```hirsel
resource type Log grant bytes 256 { sink: Sink }
```

### MEM-30: Optimisation keeps charges

No optimisation changes which operations admit, how much they charge, when they refund or
which of them fail with `Capacity`. An engine that allocates less still charges the
logical amount; an engine that elides a copy still charges for it.
