# Budgets

Every allocation is charged to a limit before anything is written, and a refusal is the
ordinary value `Capacity`. Charges are logical sizes fixed by the language, so the same
program fails at the same point on every build and every engine.

### MEM-21: Limits

A **limit** is a number of bytes with a running total of bytes used. The run's root limit
is granted to `main` by the entry ([PROG-3](programs.md#prog-3-entry)). Every other limit is
**reserved** from a parent limit: a bound scope's from the limit current where it is
entered (MEM-27), a task's from its spawner's current limit at spawn
([CONC-3](tasks.md#conc-3-spawn)). At each point of a program exactly one limit is
**current**: the innermost enclosing bound scope's, or else the task's. There is no other
budget object in v1; nothing can name a limit.

### MEM-22: Each block remembers its limit

An own block, a counted node and a region allocation are charged to the limit current
where they are made. A block keeps that limit when its owner is moved, sent, returned or
grown (MEM-28): its refund goes to the same limit. When a limit ends (its bound scope
exits or its task finishes) while blocks charged to it survive, their charges pass to the
limit's parent, and their later refunds go there; this repeats up to the root. Transferring
or refunding across tasks is an event in the run's event order
([CONC-10](tasks.md#conc-10-event-order)).

```hirsel
fn make(n: u64) -> Result[List[u64], Capacity] {
  let xs = bound bytes 1024 { list.with_capacity(n)? }?  // after the scope, xs is charged to the caller's limit
  Ok(xs)
}
```

### MEM-23: Admission

Every operation that allocates admits its charge before any write: `used + n ≤ limit`
gives `used += n`; otherwise the operation fails with `Capacity` and nothing has been
allocated ([T5.6](ownership.md#own-26-the-transition-table)). Admission is the commit point
of every allocating operation ([FAIL-3](failure.md#fail-3-commit-points)). An operation that
may allocate states it in its type: its result is `Result[_, Capacity]`, or an error type
that includes `Capacity`. No other operation allocates.

```hirsel
fn grow(mut xs: List[u64]) -> Result[(), Capacity] { list.push(mut xs, 1) }
```
```hirsel
fn grow(mut xs: List[u64]) { list.push(mut xs, 1) }         // refused: LANG-18, unused Result
```

### MEM-24: The Capacity value

`Capacity` is a plain record `{ requested: u64, available: u64 }`: the charge that was
refused and the room left in the current limit. Constructing, copying and returning it
admits nothing. Two runs of the same program with the same inputs and limits produce the
same `Capacity` values at the same points (SEM-8).

### MEM-25: Refund

A block is refunded to its limit (MEM-22) when it is freed: when its owner is cleaned,
when a builder's region closes, when a counted node's last holder goes, and when a
collection shrinks by an operation that states it. Refunding cannot fail and admits
nothing ([T5.7](ownership.md#own-26-the-transition-table)). Cleaning, closing and counted
release never allocate ([FAIL-13](failure.md#fail-13-mechanical-release)).

### MEM-26: Logical charges

A charge is the logical size of what is allocated under the policy `charge-v1`:

| allocation | charge |
|---|---|
| own record or variant block | the sum of its field sizes, variants at their largest payload, rounded up to 8 |
| `List[T]`, `Bytes`, `String` storage | capacity × element size, rounded up to 8 |
| `Map[K, V]` storage | capacity × (key size + value size + 16), rounded up to 8 |
| counted node | payload size + 8 |
| region allocation | its byte count, rounded up to 8 |
| task | its environment's size + 256 |
| channel | capacity × item size + 64 |

Scalar sizes are their widths; `bool` and `char` are 1 and 4; an own reference to a block
counts 8; `List`, `String`, `Bytes` and `Map` headers count 24. Growth that doubles capacity
does so from 4 elements. No allocator constant, alignment or chunk size of an engine enters
a charge. A later policy has a new name; a package states the policy it was written for
([PROG-1](programs.md#prog-1-packages)).

### MEM-27: The bound scope

`bound bytes n { body }` evaluates to `Result[T, Capacity]` where `T` is the body's type.
Entering it admits `n` against the current limit and makes a new limit of `n` current for
the body; if the admission fails the body does not run and the result is `Error(c)`
([T5.4](ownership.md#own-26-the-transition-table)). It covers every allocation kind made in
the body: own, counted, region and task storage. In the body, `?` applied to an
`Error(c)` of type `Capacity` exits the bound scope with `Error(c)`; `?` on any other error
type exits the function as usual. On exit by any edge, the body's
views end and its owners are cleaned, the unused part of `n` is refunded to the parent, and
the charges of surviving blocks pass to the parent (MEM-22). `bound` is the only bounding
form; a region is not one.

```hirsel
fn parse_bounded(src: String) -> Result[Result[Ast, ParseError], Capacity] {
  bound bytes 65536 { parse(src) }
}
```
```hirsel
fn f() -> Result[u64, Capacity] { bound bytes { 1 } }       // refused: LANG-5, bound needs a size
```

### MEM-28: Growth keeps the limit

Growing a collection charges the new storage to the limit of its existing block, not to the
current limit, and refunds the old storage there. A collection made outside a bound scope
and grown inside it is not charged to the scope.

### MEM-29: Finalizer grants

A finalizer cannot allocate from a limit. A resource type whose finalizer needs storage
declares `grant bytes n`; creating a value of the type admits `n` with the value, the
finalizer may allocate up to `n` from that grant only, and the grant is refunded after the
finalizer returns. An allocation beyond the grant is a finalizer failure with cause
`Capacity` ([FAIL-12](failure.md#fail-12-finalizer-failure)).

```hirsel
resource type Log grant bytes 256 { sink: Sink }
```

### MEM-30: Optimisation keeps charges

No optimisation changes which operations admit, how much they charge, when they refund or
which of them fail with `Capacity`. An engine that allocates less still charges the
logical amount; an engine that elides a copy still charges for it.
