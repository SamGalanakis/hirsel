# Budgets

### MEM-19: Limits

A **limit** is a number of bytes with a running total of bytes used. The run's root limit
is granted to `main` by the entry ([PROG-3](programs.md#prog-3-entry)). Every other limit is
**reserved** from a parent limit: a bound scope's from the limit current where it is
entered (MEM-25), a task's from its spawner's current limit at spawn
([CONC-3](tasks.md#conc-3-spawn)), a finalizer grant's with its resource (MEM-27). At each
point of a program exactly one limit is **current**: the innermost enclosing bound scope's
or running finalizer's grant, or else the task's. Nothing can name a limit, and there is no
other budget object in v1 but
the step bound of MEM-30, which counts steps, not bytes.

Charges cover every block, node, region allocation, queue, task record, cause cell and
grant a program makes. They do not cover the call stack, host memory behind provider
operations ([CAP-5](capabilities.md#cap-5-provider-operations)), or the engine's own
bookkeeping: drop flags and the limit records of MEM-20.

### MEM-20: Limit records and hand-over

Every block, counted node, region allocation, queue, task record and cause cell is charged
to the limit current where it is made, except that growth is charged where MEM-26 says,
and refers to the record of the limit it was charged to. A block keeps its record when its owner is moved, sent, returned or received: its refund goes to the
limit the record leads to.

When a limit ends (its bound scope exits, its finalizer returns, or its task finishes), the
unused part of its reservation is refunded to its parent at once, and its record is joined
to the parent's: from then on the record leads to the parent for refunds, and if the parent ends
in turn, on to its parent, up to the root. This
**hand-over** visits no value, admits nothing and cannot fail: the parent already holds the
surviving bytes as part of the reservation it gave. It covers everything the limit funded
that outlives it: a bound scope's result and anything moved outward, the cause cells of a
failure raised under it ([FAIL-10](failure.md#fail-10-cause-chain)), values in a queue, and a
task's result. So the lexical end of a limit and the death of what it funded are separate,
and the limit a block's record leads to is always live. Transferring or refunding across
tasks is an event in the run's event order ([CONC-10](tasks.md#conc-10-event-order)). An
admission sees exactly the refunds from other tasks that the order puts before the
admitting task's latest event; a refused admission is itself an event.

Records form a union-find forest, joined by rank with path compression; each tree's
representative points to the live limit it leads to, so a refund costs amortised
inverse-Ackermann time in the number of records. A record is freed once its limit has ended
and no block, reservation or record refers to it. Records are engine bookkeeping and are
not charged.

```hirsel
fn make(n: u64) -> Result[List[u64], Capacity] {
  let xs: List[u64] = bound bytes 1024 { list.with_capacity(n) }?   // after the scope, xs is the caller's
  Ok(xs)
}
```

### MEM-21: Admission

Every operation that allocates admits its charge before any write: `used + n ≤ limit`
gives `used += n`; otherwise the admission is **refused**, nothing has been allocated
([T5.6](ownership.md#own-26-the-transition-table)), and the operation raises a failure
whose primary cause is `Capacity(c)`, `c` the refused charge
([FAIL-2](failure.md#fail-2-causes)). For an allocating operation, admission is its commit
point ([FAIL-3](failure.md#fail-3-commit-points)), so a refused one has consumed nothing:
the inputs it would have taken are dropped by the failure's cleanup with the rest of the
statement ([FAIL-8](failure.md#fail-8-exit-edge-cleanup)). The failure is observed as every
failure is: by an enclosing `isolate` ([FAIL-16](failure.md#fail-16-isolation)) or by the
task's joiner ([CONC-6](tasks.md#conc-6-join)). An allocating operation's result type does
not mention `Capacity`.

Four admissions return their refusal as the value `Error(c)` instead, because the caller
recovers at the point of refusal: entering a bound scope (MEM-25), entering an `isolate`
([FAIL-16](failure.md#fail-16-isolation)), a resource literal
([FAIL-11](failure.md#fail-11-resources)), whose fields need an owner that is not a
temporary, and the core operations named `try_` that admit
([core](core.md)), such as `list.try_reserve` and `task.try_spawn`. Each says so in its
result type, and nothing else does.

```hirsel
fn grow(mut xs: List[u64]) { list.push(mut xs, 1) }        // a refusal fails here; xs unchanged
```
```hirsel unbuilt
fn room(mut xs: List[u64], n: u64) -> bool {
  match list.try_reserve(mut xs, n) { Ok(()) => true, Error(_) => false }
}
```

### MEM-22: The Capacity value

`Capacity` is a plain record `{ requested: u64, available: u64 }`: the charge that was
refused, and the room left in the limit the admission was tried against. A refused
admission carries it, in its failure's cause `Capacity(c)` (MEM-21) or in its `Error(c)`. Charges are
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
| `List[T]`, `Deque[T]`, `Heap[T]`, `String` storage | capacity × element size (`String`: 1), rounded up to 8 |
| `list.sort_stable` scratch | length × element size, rounded up to 8, refunded before it returns |
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
| connection ([CONC-17](tasks.md#conc-17-connections)) | 64 + capacity × item size + its room |
| `Latest` root | 32 |
| `log.new` node | 24 |
| a log's entries | as `List[E]` storage |
| a flow port | `cap × (16 + size(T)) + 64`, refunded to its limit record on flow cleanup ([CONC-14](tasks.md#conc-14-flows)) |
| a flow stage | `m + size(X) + 320`, refunded to its limit record on flow cleanup ([CONC-14](tasks.md#conc-14-flows)) |
| a virtual provider's port or cell ([core](core.md#provide)) | a port: its room `n`, from the constructing task's limit, refunded when the port is gone; a cell: 32, from the current limit, held until the run ends as a narrowed grant's rule storage is, since its plain capability can be copied anywhere |
| `isolate` | 32 for its cause cell, from the current limit (inside a finalizer, its grant); refunded when `body` returns, and when a caught failure leaves the current obligation's cell unused ([FAIL-16](failure.md#fail-16-isolation)) |

`list.new`, `string.new`, `map.new`, `deque.new`, `heap.new`, `list.with_capacity(0)` and an empty span builder
allocate nothing and charge 0; an explicit `span.with_capacity(r, 0)` and an empty
`text.copy_in` still admit their count header ([core](core.md)). A `clone` or `clone_in`
charges each copy at capacity equal to its length, and a compact `clone_in` charges the
selected value's bytes when its type is streamable and the whole copied blob otherwise ([MEM-17](memory.md#mem-17-clone)). An allocation not in this table does not exist in v1.

**Charge parity.** A provider operation's charges are the caller's, and they do not depend
on the provider: against a virtual provider ([core](core.md#provide)) the caller pays exactly
the admissions and steps it pays against the host for the same operations and results. Every
byte the server keeps (handle records, staged reply bytes) comes out of its port's room `n`,
which the server admitted at `provide.dir`, `provide.net`, `provide.stdout` or
`provide.stderr`; bytes a server copies out with `provide.payload` land in capacity the server
already holds. A reply that needs more room than is left raises `Capacity` in the server and
never charges the caller.

Creating a resource admits its cause cell, and can therefore be refused; then
nothing was admitted, so nothing is refunded, and the literal's fields come back in its
error ([T1.2](ownership.md#own-26-the-transition-table)). This holds unless
it is **mechanical**: its cleanup can never add a cause. These mechanical resources have no cause cell: `Sender`, `Receiver`, `Task`,
`Latest`, `Client`, `Inbox`, `Flow`, `HostFile` and the four socket resources described in
[Net](net.md). The channel and task handles close a channel or end a child. A `Latest`
handle releases its root. If cleaning a queued item or result raises a cause, the
value's finalizer owns that cause. A `Flow` discards its entries and refunds every
charge during cleanup. Its stages run no user cleanup ([CONC-14](tasks.md#conc-14-flows)).
`HostFile` closes the host file and ignores the outcome. Unbuilt: `Proc`, `UnixListener`
and `UnixDatagram` are mechanical too ([EXEC-3](exec.md#exec-3-spawn),
[NET-13](net.md#net-13-unix-streams), [NET-14](net.md#net-14-unix-datagrams)). A child's pipe
ends are the existing `ReadHalf` and `WriteHalf`. The supervisor's records and the host's
descriptors are provider storage, outside every limit. A served reply that issues several ids
reserves all their room from the server's port at once, or fails with `Capacity` having
issued none. A narrowed `Exec` or `Unix` rule's storage is the logical size that
[EXEC-2](exec.md#exec-2-launch-profiles-and-narrowing) and
[NET-12](net.md#net-12-unix-rules) define. Other resources admit their
cells. `channel.sender` and `latest.handle` admit nothing.

Sizes are logical, never an engine's: scalars are their widths, `bool` 1, `char` 4, `()` 0; a
record or tuple is the sum of its fields; a variant type, plain or own, is 8 plus its largest
payload, whatever its layout, a tag kept in a payload's spare values
([MEM-13](memory.md#mem-13-own-layout)) included; a value of a declared resource type counts 8 wherever it is held, its fields, cell
and grant being charged once on its own block; a plain `fn` counts 8, and an `own fn`, a
`scoped fn` and a reference to a region value count 16; a compact value held inline counts
its encoding. Every built-in type's size is in [core](core.md#built-in-types). Words an
engine adds of its own, such as alignment or an environment's drop recipe, are never
charged. A charge is computed from the type by these rules and is the same on every target
and under every layout choice; the bytes a value occupies may be fewer: `Option[Box[T]]`
charges 8 + 8 while an engine may hold it in 8 bytes.

A collection grows to capacity `max(needed, 4, 2 × old capacity)`; the new storage is
admitted before the old is refunded, so growth briefly holds both. Nothing shrinks
storage automatically; `with_capacity(n)` admits exactly `n` elements. No allocator
constant, alignment or chunk size of an engine enters a charge. A later policy has a new
name; a package states the policy it was written for
([PROG-1](programs.md#prog-1-packages)).

### MEM-25: The bound scope

`bound bytes n { body }` sets a byte limit for its body and evaluates to
`Result[T, Capacity]` where `T` is the body's type. Entering it admits `n` against the
current limit and makes a new limit of `n` current for the body; if that admission is
refused the body does not run and the result is `Error(c)`
([T5.4](ownership.md#own-26-the-transition-table)). That entry refusal is the only `Error`
the scope makes: when the body runs, the result is `Ok` of its value. The limit covers every
allocation kind made in the body: own, counted, region and task storage. An admission the
body's limit refuses raises the failure `Capacity(c)` (MEM-21), which the scope does not
catch: it propagates past the scope, cleaning it as every exit edge does, to an enclosing
`isolate` or the task's joiner. So a refused entry (nothing ran) and a body that ran out
(its effects stand) stay distinct. `?` in the body exits the function as it does anywhere
([LANG-18](language.md#lang-18-errors)). On exit by any edge, the body's views end and its
owners are cleaned, the unused part of `n` is refunded to the parent, and the record is
handed over (MEM-20). `bound bytes` is the only form that bounds bytes; a region is not
one, and `bound steps` (MEM-30) bounds steps. Because the scope catches nothing, its body
may assign and lend `mut` places declared outside it.

```hirsel
fn copy_bounded(src: String) -> Result[String, Capacity] {
  bound bytes 65536 { string.from(src) }   // Error only when the 65536 is refused
}
```
```hirsel
fn fill(mut xs: List[u64], n: u64) -> Result[(), Capacity] {
  bound bytes 4096 { list.fill(mut xs, n, 0) }   // past 4096 bytes: the failure Capacity
}
```
```hirsel
fn f() -> Result[u64, Capacity] { bound bytes { 1 } }       // refused: LANG-5, bound needs a size
```

### MEM-26: Growth charges the current limit

Growing a collection admits its new storage from the current limit and refunds the old storage to the
limit its record leads to. The new storage refers to the current limit's record. Growth briefly holds
both (MEM-24). A refusal leaves the collection and its old charge unchanged.

### MEM-27: Finalizer grants

A resource type whose finalizer allocates, creates resources or spawns declares
`grant bytes n`; one without a declaration has a grant of 0. Creating a value of the type
reserves the grant as part of its charge (MEM-24). The grant is a limit of `n` bytes, and it
is the current limit while the finalizer runs: its allocations, and the storage, cause cell
and grant of every resource or task it creates, are admitted from it. An admission it
refuses raises `Capacity(c)` as everywhere (MEM-21); in a finalizer that is a finalizer
failure ([FAIL-12](failure.md#fail-12-finalizer-failure)), recorded in the resource's own
cause cell ([FAIL-10](failure.md#fail-10-cause-chain)), and abandons the finalizer body. The
four value refusals of MEM-21 stay values under a grant too. When the finalizer ends, the unused part of the grant is
refunded, and what survives it (cause cells, sent values) is handed over to the limit that
funded the resource (MEM-20). So cleanup, failure cleanup included, never admits from any
limit but a grant.

While the finalizer runs, the grant's record belongs to the task running it; when the finalizer returns
in another task than the one that created the resource, handing the grant's unused part and records back
is an event.

```hirsel
resource type Log grant bytes 256 { sink: Sender[String] }
```

### MEM-28: Optimisation keeps charges

No optimisation changes which operations admit, how much they charge, which refunds an
admission sees or which of them are refused
([SEM-8](semantics.md#sem-8-determinism)). An engine that allocates less still charges the
logical amount; an engine that elides a copy still charges for it.

### MEM-30: Step bounds

A **step** is taken at each call, each loop back edge and each other entry of user code,
and nowhere else; a **step
bound** of `n` allows `n` steps, and the step after them stops the code with the failure
`OutOfTime` (FAIL-2), which the scope whose bound ran out catches; no `isolate` catches it
while that bound is current. `bound steps n
{ body }` evaluates to `Result[T, OutOfTime]`, where `T` is the body's type and `OutOfTime`
is core's plain record `{ steps: u64 }`.

- **Steps.** A call expression takes one step when its call is entered, after its arguments
  are evaluated: a call of a function, of a callable value or of a core function. A loop
  takes one step each time control returns to its head: at the end of a `loop`, `while` or
  `for` body, the last time included, and at `continue`. Entering a loop, and leaving it by
  `break`, `return` or failure, takes none, so `while i < n { i += 1 }` from 0 takes n steps.
  User code entered without a call expression takes one step on entry: a finalizer when
  cleanup runs it, and a callable a core function calls back (a callable argument, a codec's
  `via`). Iteration (SEM-6), operators, field and index access, literals and constructors,
  `?`, patterns and `match` run no user code and take none. A core function's call, and a
  foreign function's ([FFI-7](foreign.md#ffi-7-host-stack-and-waits)), is one
  step whatever it does; nothing inside it is counted but the user code it enters. The one
  exception is `secret.scram_sha256`, which takes one further step for each PBKDF2 iteration
  it admits, at the call and before any derivation, because its work is chosen by an untrusted
  server, not by sizes a limit bounds ([TLS-6](tls.md#tls-6-scram-sha-256)). Code an
  engine adds of its own, such as drop, clone and growth code, takes none. So the steps a run
  takes follow from its source and its values alone.
- **The current bound.** At each point exactly one step bound is current, or none: the
  innermost enclosing `bound steps` scope's, else the task's root step bound, else none.
  Every step taken while a bound is current uses one of it. Entering `bound steps n`
  reserves `n` from the current bound, if there is one; when `n` exceeds what it has left,
  the body does not run and the result is `Error(OutOfTime(steps: n))`. Otherwise a new
  bound of `n` is current for the body. On every exit edge the unused part returns to the
  bound current at entry, so an inner scope's steps count against every enclosing bound.
  Nothing can name a bound.
- **Exhaustion.** When a step would use more than the current bound allows, it does not
  happen: a failure with primary cause `OutOfTime` is raised there, its site the call or
  the loop. It propagates as every failure does, cleaning each scope it leaves (FAIL-8), and
  no `isolate` it passes while the bound that ran out is current catches it
  ([FAIL-16](failure.md#fail-16-isolation)). The bound stays
  current and exhausted during that cleanup, so a finalizer it would run fails on entry
  with `OutOfTime` ([FAIL-12](failure.md#fail-12-finalizer-failure)): it is abandoned, its
  resource's fields are still cleaned mechanically, and cleanup goes on, so it ends. When the failure reaches the scope
  whose bound ran out and every cause in its chain is that bound's `OutOfTime`, the scope
  catches it: their cells are refunded and it returns `Error(OutOfTime(steps: n))`. If
  cleanup added any other cause, the failure goes on past the scope, as a failure in its
  place would: from there that bound is not current, the failure is an ordinary one, and an
  `isolate` it reaches catches it, `OutOfTime` first among its causes.
- **Places outside.** The body is checked as `isolate`'s argument is (FAIL-16): it is
  refused when it assigns, replaces or lends `mut` a place declared outside it, or invokes a
  scoped callback whose descriptor it cannot see. A place it moves from is maybe moved after
  the scope. So a stop leaves every place outside the scope as it was at entry, except
  effects that do not install into a place (FAIL-16's list).
- **Waits, bytes and tasks.** Waiting takes no steps: a step bound limits computation, not
  time, and a body that waits forever is ended by a deadline or a cancellation
  ([CONC-7](tasks.md#conc-7-cancellation-points)), not by its bound. Step bounds and limits
  are independent and nest in either order; a step bound admits nothing and charges
  nothing. Because a core call is one step, a step bound bounds time only together with the
  limits that bound the sizes core calls work on.
  A task spawned while a bound is current runs under its own root bound of what that bound has left at the
  spawn; the spawner's bound is not reduced, and no steps return. No two tasks share a bound, so where a
  bound stops a task follows from that task's source and values alone. The steps a run takes under a bound
  of `n` are at most `n` for each task spawned under it, and the limits that admit those tasks bound how
  many there are. `main` and a task spawned with no bound current take steps under none.
  A flow stage ([CONC-14](tasks.md#conc-14-flows)) copies the owner's remaining steps at
  `flow.stage`. Each item uses a fresh bound of `k` within that copy. The copy refuses
  entry when fewer than `k` remain. The stage state is an input value of this bound.
- **Slices.** While a step bound is current, every 65,536th step a task takes under it is a
  **slice end**: an event ([CONC-10](tasks.md#conc-10-event-order)) at which the scheduler may
  run another task. A flow stage has no event at its slice end. A task's slice end observes
  no cancellation and changes nothing else, so code under a
  bound that computes for long does not keep the tasks it does not wait for from running.
- **Determinism.** Which steps a run takes, where a bound stops it and where its slices end
  depend only on its source and the values at its replay point
  ([SEM-8](semantics.md#sem-8-determinism)): every engine, build and optimisation level
  stops at the same step. No
  optimisation adds, removes, merges or moves a step taken while a bound is current; an
  engine may count several steps at once only where it then stops at the same step as
  counting one at a time would. A step taken while no bound is current uses nothing and
  ends no slice, so an engine need not count it, even in code that also runs under a bound.

```hirsel
fn spin(n: u64) -> Result[u64, OutOfTime] {
  bound steps 1000 { let mut i: u64 = 0; while i < n { i += 1 }; i }   // Error(OutOfTime(steps: 1000)) when n > 1000
}
```
```hirsel
fn first(xs: List[u64]) -> Result[Option[View[u64]], OutOfTime] in xs {
  bound steps 1 { list.get(xs, 0) }    // one call: one step
}
```
```hirsel
fn tries(n: u64) -> Result[u64, OutOfTime] {
  bound steps 1000 {
    match isolate(fn() -> u64 { let mut i: u64 = 0; while i < n { i += 1 }; i }) {
      Ok(Ok(i)) => i,
      _ => 0,                          // never reached by the bound's OutOfTime, which passes this isolate
    }
  }                                    // Error(OutOfTime(steps: 1000)) when the loop runs out
}
```
```hirsel
fn bump(mut n: u64) -> Result[(), OutOfTime] {
  bound steps 10 { n = n + 1 }         // refused: MEM-30, n is declared outside the scope
}
```
