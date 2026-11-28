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
bookkeeping: drop flags and the limit records of MEM-20. The one provider memory they do
cover is a `UdpSocket`'s receive pool and a group's queues, whose size the program chooses
([NET-16](net.md#net-16-udp-sockets-and-batches)). That charge is refunded at the pool's
recorded retirement, not at the drop, so its credit never funds new storage while the old
storage is live. A limit that ends before the retirement hands the charge over to its parent
like any surviving charge (MEM-20), so a helper's join does not refund it
([CONC-15](tasks.md#conc-15-units)). The kernel's socket queues behind a `UdpSocket` are not
charged here. The run's UDP host allowance admits them nominally, at each socket's default
sizes read at run start. That is a per-run count, not a ceiling on kernel memory (NET-16), and
a refusal there is a provider error, not a refused admission.

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
task's result. A virtual provider's Clock, Random or Process cell is not handed over: it is
freed when the limit it was charged to ends (MEM-24). So the lexical end of a limit and the death of what it funded are separate,
and the limit a block's record leads to is always live. Transferring or refunding across
tasks is an event in the unit's event order ([CONC-10](tasks.md#conc-10-event-order)); one
sent to another unit is an input of that unit ([CONC-15](tasks.md#conc-15-units)). An
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
```hirsel
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
same build with the same inputs and limits produce the same `Capacity` values at the
same points ([SEM-8](semantics.md#sem-8-determinism)). Both fields are diagnostic: a build that
charges less than the logical charge (MEM-28) reports what it charged, so another build can
report other values or refuse at another point.

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

A **logical charge** is the logical size of what is allocated under the policy `charge-v1`. It
is the most any build charges for the allocation (MEM-28):

| allocation | charge |
|---|---|
| `Box[T]` node | the payload's size, rounded up to 8 |
| `List[T]`, `Deque[T]`, `Heap[T]`, `String` storage | capacity × element size (`String`: 1), rounded up to 8 |
| `Columns[T]` storage | as `List[T]`: capacity × size(T), rounded up to 8, one block holding every column |
| `list.sort_stable` scratch | at most length × element size, rounded up to 8, refunded before it returns; how much scratch, if any, is the build's ([core](core.md)) |
| `Map[K, V]` storage | capacity × (key size + value size + 16), rounded up to 8 |
| counted node | payload size + 8, rounded up to 8 |
| owning closure environment | the sum of its captures' sizes, rounded up to 8 |
| region allocation | its byte count, rounded up to 8 |
| `UdpSocket` receive pool ([NET-16](net.md#net-16-udp-sockets-and-batches)) | slots × slot bytes; a group: (members + 1) × slots × slot bytes; refunded at retirement |
| a `Text` or `FixedSpan` made outside a compact type | 8 (count) + payload bytes, rounded up to 8 |
| a span builder's allocation | a region allocation of its current capacity × element size + 8 |
| a `build_in` or `build_stream_in` allocation | `n`, rounded up to 8; after `finish_trim`, its used bytes, rounded up to 8 |
| `seq.index_in` | 24 + count × 8, rounded up to 8 |
| resource value | one block: its fields' sizes, plus 32 for its cause cell, plus its grant (MEM-27) |
| task | 256 + its result's size + 32 for its primary cause cell + its reserved limit `n` |
| `main`'s task at startup | 256 + 32, from the root limit before `main` runs; a root limit smaller than this does not start the run |
| channel | capacity × item size + 64 |
| connection ([CONC-17](tasks.md#conc-17-connections)) | 64 + capacity × item size + its room, up to `2^22 − 1` slots and `2^40 − 1` bytes of room |
| `Latest` root | 32 |
| `Shared` node ([CONC-15](tasks.md#conc-15-units)) | the payload's size + 40, rounded up to 8; the buffers the payload holds keep their own charges |
| `log.new` | its cell's node: 24, the `List[E]` carrier; its entries as `List[E]` storage |
| `cell.new` node | `T`'s size, rounded up to 8, as a `Box[T]` node; the blocks `T` holds keep their own charges |
| a flow port | `cap × (16 + size(T)) + 64`, refunded to its limit record on flow cleanup ([CONC-14](tasks.md#conc-14-flows)) |
| a flow stage | `m + size(X) + 320`, refunded to its limit record on flow cleanup ([CONC-14](tasks.md#conc-14-flows)) |
| a virtual provider's port or cell ([core](core.md#provide)) | a port: 32 and its room `n`; a Clock, Random or Process cell: 32; each from the current limit, refunded when that limit ends, by a fault too, which withdraws the cell: a copy of its plain capability that outlives the limit designates a withdrawn provider ([core](core.md#provide)) |
| a packet key ([TLS-8](tls.md#tls-8-packet-ciphers-and-key-agreement)) | the row TLS-8's charge table gives it: a `Schedule` 512, from the current limit at each constructor and `srtp`, refunded at drop; a `KeyShare` 64; a `Sealer` or `Opener` 1024; a `HeaderMask` 512; a registry entry 48; a ledger entry 16; `p256_key`'s document 176; the AAD scratch its span's length plus `extra`, refunded at the call's return |
| `isolate` | 32 for its cause cell, from the current limit (inside a finalizer, its grant); refunded when `body` returns, and when a caught failure leaves the current obligation's cell unused ([FAIL-16](failure.md#fail-16-isolation)) |

`list.new`, `string.new`, `map.new`, `deque.new`, `heap.new`, `list.with_capacity(0)` and an empty span builder
allocate nothing and charge 0; an explicit `span.with_capacity(r, 0)` and an empty
`text.copy_in` still admit their count header ([core](core.md)). A `clone` or `clone_in`
charges each copy at capacity equal to its length, and a compact `clone_in` charges the
selected value's bytes when its type is streamable and the whole copied blob otherwise ([MEM-17](memory.md#mem-17-clone)). An allocation not in this table does not exist in v1.

**Charge parity.** A provider operation's charges are the caller's, and they do not depend
on the provider: against a virtual provider ([core](core.md#provide)) the caller pays exactly
the admissions and steps it pays against the host for the same operations and results. Every
byte the server keeps (handle records, staged reply bytes, staged UDP packet records) comes out of its port's room `n`,
which the server admitted at `provide.dir`, `provide.net`, `provide.stdout` or
`provide.stderr` or `provide.exec`. Bytes a server copies out with `provide.payload` land in capacity the server
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
`HostFile` closes the host file and ignores the outcome. `Proc`, `UnixListener`
and `UnixDatagram` are mechanical too ([EXEC-3](exec.md#exec-3-spawn),
[NET-13](net.md#net-13-unix-streams), [NET-14](net.md#net-14-unix-datagrams)). A child's pipe
ends are the existing `ReadHalf` and `WriteHalf`. Spawn admits eight bytes for the `Proc`
and for each pipe half together. Each drop refunds its eight bytes. The supervisor's records
and the host's descriptors are provider storage, outside every limit. A served reply that issues several ids
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
charged. A logical charge is computed from the type by these rules and is the same on every target
and under every layout choice; the bytes a value occupies may be fewer: `Option[Box[T]]`
charges 8 + 8 while an engine may hold it in 8 bytes.

A collection grows to capacity `max(needed, 4, 2 × old capacity)`; the new storage is
admitted before the old is refunded, so growth briefly holds both. A push within capacity
admits nothing. Nothing shrinks
storage automatically; `with_capacity(n)` admits exactly `n` elements. No allocator
constant, alignment or chunk size of an engine enters a charge. A later policy has a new
name; a package states the policy it was written for
([PROG-1](programs.md#prog-1-packages)).

A call to `std.data.decode` temporarily admits 16 bytes per byte of its compiler
witness before opening it. If the input fits `input_bytes`, parsing admits 128
bytes per input byte plus 128. Each record visited admits another 128 bytes per
declared field. Construction admits each staged block's byte length plus a
96-byte journal entry, including the inline result. These reservations stay
with the current payer until the call returns or its task is torn down. Owned
strings, lists and maps admit their normal collection charges before publication.
On failure the operation refunds staged allocations and leaves the result slot
uninitialized. A document error is an initialized `Error(DataError)` result.

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

### MEM-28: Charges are an upper bound

A build charges each allocation at most its logical charge (MEM-24) and at least the logical
charge of the block it really makes. An allocation a build does not make charges 0: a
`FrozenBytes` literal held in read-only static storage ([core](core.md)), a copy the build
elides, a scratch block it does without. Charges are deterministic per build: every run of
one build admits at the same operations, charges the same amounts, sees the same refunds and
is refused at the same points ([SEM-8](semantics.md#sem-8-determinism)). Across builds they
are a safety contract, not an observable to match: a body that fits under `bound bytes n` by
its logical charges fits on every engine and at every optimisation level, and a build may
admit what the logical charges would refuse.

### MEM-33: Reading a logical charge

A program reads the logical charges of MEM-24 from the
type, never from a number it copied. Two pieces do it, and nothing else prices a type.

**`bytes[T]`** is an expression of type `u64` whose value is `T`'s logical size under the
package's charge policy (the "Sizes are logical" paragraph of MEM-24): the size a field, an
element, a channel item or a task result of type `T` counts. It is a primary
([LANG-5](syntax.md#lang-5-grammar)) and a constant: it may appear in a `const` initializer
([LANG-20](language.md#lang-20-constants)), it costs no step and charges nothing. A constant whose `bytes[T]` reaches the constant itself, through an `Inline` length in `T`, is refused as LANG-20's other cycles are. `T` is any
complete type, `bytes[T]` in a generic body included: there it is `T`'s size at each
instantiation, exactly as `channel.new(n)` in that body charges `n` × that size. An opaque type
of another package is accepted, because its size is already observable through every charge
that counts it; opacity hides fields, not size ([PROG-2](programs.md#prog-2-visibility)). A
resource type counts 8 here, as wherever it is held; its block is priced by `charge.block`
below. `bytes[T]` is a source of truth for what a type counts and never a prediction of an
engine's layout: an engine may hold fewer bytes (MEM-28). `bytes` is a keyword, so the form
is always written bare: `charge.bytes[T]` or `core.bytes[T]` is refused (LANG-2) with a
diagnostic naming `bytes[T]`.

```hirsel
type Pair { a: u64, b: u32 }
const PAIR: u64 = bytes[Pair]                                // 12
const SLOT: u64 = bytes[Option[Pair]]                        // 20: 8 + 12
fn item[T](n: u64) -> u64 { n * bytes[T] }
```
```hirsel
const BAD: u64 = bytes[List]                                 // refused: LANG-12, List takes a type argument
```

**The core module `charge`** holds MEM-24's recipes as functions of sizes and counts. They
are the only place in any package where a recipe's own constants (the 256-byte task record,
the 32-byte cause cell, the 64-byte channel base, Map's 16-byte entry overhead, the
rounding to 8) are written, and each returns `None` on overflow:

```hirsel core
pub fn round(n: u64) -> Option[u64]                                    // up to a multiple of 8
pub fn boxed(payload: u64) -> Option[u64]                              // Box[T]: round(payload)
pub fn counted(payload: u64) -> Option[u64]                            // Rc, Arc, Unique node: round(payload + 8)
pub fn shared(payload: u64) -> Option[u64]                             // Shared node: round(payload + 40)
pub fn seq(element: u64, capacity: u64) -> Option[u64]                 // List, Deque, Heap, String storage
pub fn map(key: u64, value: u64, capacity: u64) -> Option[u64]        // Map storage
pub fn channel(item: u64, capacity: u64) -> Option[u64]                // 64 + capacity × item
pub fn task(result: u64, limit: u64) -> Option[u64]                    // 256 + result + 32 + limit
pub fn block(fields: u64, grant: u64) -> Option[u64]                   // a resource value's block: fields + 32 + grant
pub fn port(item: u64, capacity: u64) -> Option[u64]                   // a flow port
pub fn latest() -> Option[u64]                                       // a Latest root
```

A price is then the recipe applied to the type: `charge.channel(bytes[Ack], 3 * g + 3)`,
`charge.task(bytes[Result[(), Error]], grant)`. These are ordinary functions, so a derived
price is a `fn` or a `let`, or a `const`, which LANG-20 evaluates as a computed constant. Their results are
exactly MEM-24's rows; a later policy (MEM-24's last paragraph) changes the module and
`bytes`, and no caller.

**Who computes what.** A price that spans packages splits in exactly one way:
- The **provider** of a value or a service derives the charge of **one owner** of it from
  its own types, with `bytes` and `charge`, and publishes it as a function or a plan. A
  provider's figure is never restated by a caller, in source or in a law.
- The **caller** keeps the **number of simultaneous owners**, and multiplies or sums the
  provider's figure by it with checked arithmetic. A caller that holds two stores asks the
  provider for one store's price and doubles it; it does not know what a store holds.
- A **law** pins a figure only in the package that derives it, against a hand derivation
  from the recipes. A caller's law pins its own terms and its owner counts, and composes
  them with the provider's published figure, so a provider's change moves no caller's
  literal.

Prior art:

| system | what it exposes | verdict |
|---|---|---|
| Rust `core::mem::size_of::<T>()`, `Layout::new::<T>()`, `Layout::array::<T>(n)` | a `const fn` for a type's size and a checked layout calculator for arrays and extension | **ADAPTED.** A size readable in constants and generic code, and a checked composite (`Layout::array` returns `Err` on overflow, as `charge.*` returns `None`), are taken. Rust's figure is the target's layout, padding and alignment included, and differs by target; `bytes[T]` is the logical size, the same on every target. Rust has no recipe for a channel's or a task's cost: `std::sync::mpsc` and `tokio` allocate behind the call and no budget sees it, which MEM-24 exists to avoid. |
| Zig `@sizeOf(T)`, `@alignOf(T)`, `std.mem.Allocator` byte counts | a builtin read at compile time; allocators take explicit byte counts | **ADAPTED.** A keyword-introduced form with a type operand, legal in comptime code, is the shape `bytes[T]` takes, since Hirsel never writes type arguments at a call (LANG-12) and `bytes` is already a keyword. Zig's figure is again the target's layout. Zig's allocator interface asks the caller for raw byte counts and leaves composite costs to each caller, the hand-copying this rule removes. |
| Goose (`/tmp/ref-goose` at `d5ae5bb`) | no type-size expression in its spec, reference or stdlib docs (goose goose_spec.md, reference.md, stdlib.md); its tutorial measures C++ and Rust with `sizeof` and reads Goose's own sizes off packed declarations (goose tutorial.md:732) | **NOT TAKEN.** Its sizes are fixed by its packed layouts and are written beside declarations, which is the hand-copying this rule replaces. Its stdlib's explicit-cost stance (adopted in [HTTP](http.md)) is kept: every price is still explicit, only its source changes. |

Reversible: before 1.0, the form, the module and the split are each spec and library text.

### MEM-30: Step bounds

In the reference count (Determinism, below), a **step** is taken at each call, each loop
back edge and each other entry of user code, and nowhere else; a **step
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
  `?`, patterns and `match` run no user code and take none, beyond the work and depth steps
  of `==` and ordering and the work steps of a repeat literal (below). A call of `list.prefetch` or
  `inline.prefetch` takes none either, since a hint must not fail
  ([core](core.md#contracts-the-signatures-do-not-state)). Any other core function's call, and a
  foreign function's ([FFI-7](foreign.md#ffi-7-host-stack-and-waits)), is one
  step plus any steps taken by the user code it enters. A contained foreign call also takes
  one step for each 16 KiB, or part, of each byte loan it copies in and again of each it copies
  back, at the call and before any copy, and one step for each 16 units of Wasm fuel it
  consumes; its slice ends and exhaustion fall inside it
  ([FFI-13](foreign.md#ffi-13-instances-bounds-and-steps)). `secret.scram_sha256` takes
  one further step for each PBKDF2 iteration it admits, at the call and before any
  derivation, because an untrusted server chooses the work ([TLS-6](tls.md#tls-6-scram-sha-256)).
  `argon2.id` and `secret.derive_argon2id` take one further step for each Argon2 block
  compression their parameters choose, at the call and before any work, because a stored
  hash or a caller names a cost that no limit bounds ([core](core.md#argon2id)).
  The operations in the work-step table (below) take their work steps. `host.udp_recv` takes one further step for each 64 entries, or part of 64, of
  its `out` spare capacity, and one for each 16 KiB, or part, of the most it can deliver
  (`min(r, e × (slot_bytes − 256))`). `host.udp_send` takes one for each 64 packets, or part
  of 64, of its `pkts`, and after validating them one for each 16 KiB, or part, of the sum of
  their lengths. The product and the sum are exact (128-bit) before rounding, so neither
  wraps. All are taken at the call and before issue
  ([NET-16](net.md#net-16-udp-sockets-and-batches)). Drop and growth take no steps: a drop
  frees what a construction or a clone already paid for, and a growth is amortized into the
  appends that cause it (each appended byte is counted once, under Work steps). A clone takes
  work steps (below). This is the **reference count**, the count the reference evaluator
  takes; a build may count otherwise (Determinism, below).
- **Work steps.** An operation whose work grows with its input takes further steps, its
  **work steps**, from one table: each row names the operation's **units**, its **quantum**
  (a power of two) and its **weight**. The operation takes `ceil(u / quantum) + weight`
  steps, `u` the exact (128-bit) sum of its units (a clone charges each row it names
  separately), before any of its work and after the
  refusals it decides before issue (`Bounds`, `Denied`, `Invalid`, a spare or a reserve too
  small). The formula serves every row; no operation has its own. A quantum is chosen so
  that one step stands for at most a fixed time on the slowest input the row admits (for
  UTF-8, the multi-byte encodings; for structural `==`, one-byte leaves), the same target
  for every row, and a weight is that time's multiple for the operation's fixed work, both
  rounded to a power of two. The table is part of the language, the same on every target
  and engine; units count logical sizes ([MEM-24](#mem-24-logical-charges)), never a build's layout.

  | operation | units | quantum | weight |
  |---|---|---|---|
  | runtime array repeat `[value; count]`; `list.append`, `copy_from_slice`, `copy_within`, `fill`, `fill_slice`, `compare_bytes`; `inline.copy_from_slice`, `copy_within`; `string.push`; `==` and ordering on `List[u8]`, `String` and `Bytes` | bytes moved, filled or compared (both operands for a comparison) | 4096 | 0 |
  | a clone, at the call | the value's own bytes | 4096 | 0 |
  | a clone, for each sequence, `Bytes` or column set it copies | its element (row) bytes | 4096 | 0 |
  | a clone, for each such sequence whose elements own storage | its length | 4 | 0 |
  | a clone, for each box it copies | 1 | 4 | 0 |
  | structural `==` and ordering (derived) | the value's bytes plus one, and for a list, `min` of the lengths times (element bytes plus one); a zero-sized element counts one | 32 | 0 |
  | `list.sum`, the elements `deque.remove` shifts | elements | 32 | 0 |
  | `columns.fill` | elements times (row bytes plus one) | 64 | 0 |
  | `std.data.decode`, compiler witness | witness bytes | 32 | 1 |
  | `std.data.decode`, source | input bytes if within `input_bytes`, otherwise zero | 1 | 0 |
  | `int.parse` | input bytes | 16 | 0 |
  | `string.from_utf8`, its ASCII prefix | bytes | 256 | 0 |
  | `string.from_utf8`, past the prefix | bytes | 8 | 0 |
  | base64 encoding | input bytes | 8 | 0 |
  | ASCII case mapping | bytes | 64 | 0 |
  | `crc32c.update` | `input` | 4096 | 0 |
  | `crc64nvme.update`, `check.xxh3_stripes` | `input`, `b` | 2048 | 0 |
  | `sha1.blocks`, `sha1.compress`, `sha256.blocks` | bytes (`64 × count` for blocks) | 256 | 0 |
  | `sha512.blocks` | `128 × count` | 32 | 0 |
  | `secret.aead_seal`, `aead_open`, `wrap`, `unwrap` | `aad` plus `plain` or `sealed`; the bytes of `s`; `blob` | 256 | 1 |
  | `secret.hmac_sha1`, its verify and JWS forms | `data` | 64 | 2 |
  | `secret.hmac_sha256`, its verify and JWS forms | `data` | 256 | 1 |
  | `secret.hmac_sha384`, `hmac_sha512`, their verify and JWS forms | `data` | 64 | 4 |
  | `ed25519.verify`; `secret.sign` with `Ed25519` | `msg` | 64 | 256 |
  | `p256.verify`, `verify_der` | `msg` | 256 | 512 |
  | `secret.sign` with `P256` | `msg` | 256 | 128 |
  | `p384.verify`, `verify_der` | `msg` | 64 | 8192 |
  | `rsa.verify_pkcs1_sha256`, `verify_pss_sha256` (to an 8192-bit modulus) | `msg` | 256 | 2048 |
  | `secret.sign` with `Rs256` (to a 4096-bit modulus) | `msg` | 256 | 32768 |

  A runtime repeat evaluates its element once, even at count zero. Before the first store
  it takes the copy row's work steps for `count × logical size of the element`. A zero count
  or a zero-width element requires no fill work.

  Empty input takes only the weight. A core function whose body performs operations of the
  table takes their work steps as well as its own row's: `string.encode_base64` takes the
  base64 row over its input and the copy row over the bytes it appends. `string.from_utf8`
  takes the prefix row over the pure ASCII bytes it starts with and the UTF-8 row over every
  byte after them. A call on a fixed-size `Inline` (`sha1.block`,
  `sha256.block`, `sha512.block`, `check.xxh3_stripe`, `check.xxh3_short_inline`) and
  `check.xxh3_short`, whose input is at most 240 bytes, does work its types bound and takes
  none. Not yet counted, each a named gap: `==` at a type parameter counts only its fixed
  part; a sequence nested in a derived `==` (the bytes of a `List[String]`'s elements, a list
  in a record) counts its element only; map key hashing and equality, beyond their depth
  steps; key derivation; the
  copy of a large `Inline` by assignment; `==` on `Deque` and `Map`; and core-source loops
  outside the table (`string.find` and its kin). A closure written in core source takes a
  step at each entry, as any closure does, so `from_utf8`'s scalar tail over multibyte text
  takes those steps besides its row.
- **Depth steps.** A walk the engine generates over a value whose depth the value chooses
  takes a step at each level, so a bound of `n` stops it with `OutOfTime` after at most `n`
  levels, before the depth can exhaust the stack ([FAIL-15](failure.md#fail-15-run-stops)).
  At a **recursive** type, one a value of which can hold another value of the same type
  through any field (a box, a counted node, a list element, a variant payload), each visit
  of such a value by derived `==`, ordering, and a map's key hashing and equality takes one
  step on entry, before any of its fields is read; a visit of a value of a type that is not
  recursive takes none. A clone's work steps (below) bound its depth the same way: a box it copies takes one, and
  a sequence of `k` owning elements `ceil(k / 4)`, so every level takes at least one. A codec's
  encoder and decoder are generated code ([SEM-10](semantics.md#sem-10-codecs)), and each
  value they visit is entered by a call, which takes its step. Drop takes none: it frees in
  constant stack.
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
  effects that do not install into a place (FAIL-16's list). To bound code that changes an
  outer place, move a working value into the scope and return it with the result, and
  keep in the place a copy made before entry: a return installs the working value, and a
  stop drops it, cleaning it as every failure does, so the place holds the copy. The copy's
  steps are taken before entry and count against the enclosing bound, not against `n`.
  The store's `rollback_writes_on_overrun(mut tx, n, body)` does this for a transaction:
  an overrun rolls back the writes `body` staged, and the transaction goes on. Its costs
  and bounds are the store's ([STORE-SPEC §1.3](../../core/programs/store/STORE-SPEC.md#13-transactions)).
- **Waits, bytes and tasks.** Waiting takes no steps: a step bound limits computation, not
  time, and a body that waits forever is ended by a deadline or a cancellation
  ([CONC-7](tasks.md#conc-7-cancellation-points)), not by its bound. Step bounds and limits
  are independent and nest in either order; a step bound admits nothing and charges
  nothing. A core call outside the work-step table is one step, so a step bound bounds its
  time only together with the limits that bound the sizes it works on.
  A task spawned by `task.spawn` or `task.try_spawn` while a bound is current **shares** that bound:
  its root bound is the spawner's current bound itself, not a copy, so its steps and the spawner's
  use one count, and every bound enclosing that one counts them too. The steps all the tasks sharing
  a bound of `n` take are at most `n`, so a spawn-then-continue whose aggregate work fits completes,
  and one whose work does not fit fails `OutOfTime` in whichever sharing task would take the step
  past the bound. One worker runs a unit's tasks ([CONC-10](tasks.md#conc-10-event-order)), so one
  task spends at a time. Before a task runs it reserves at most what is left of its slice and of the
  bound, and before any other task of the unit runs, and before it waits, ends or changes its
  current bound, it returns what it did not spend. A task that is not running holds none of a
  shared bound's steps, and no bound is overrun, not even by one step. Which task runs next is the
  scheduler's choice (CONC-10), so which sharing task spends the last step, and fails, follows the
  unit's event order: with the same build, inputs and scheduler choices it is the same task at the
  same step, and another schedule may stop another task.
  A helper started by `task.spawn_unit` while a bound is current is another unit and shares nothing
  ([CONC-15](tasks.md#conc-15-units)). Once the spawn is admitted, the helper's root bound is
  **granted** half of what the current bound has left, rounded down (possibly zero), and the
  spawner's bound keeps the rest. When the helper's handle is joined, or cleaned unjoined, the
  steps the helper left unused, its **residual**, return once to the bound that granted them,
  whether the helper returned, failed or was cancelled. A helper's end that arrives earlier gives
  the spawner nothing to spend, and a refused `spawn_unit` takes no steps. `main` and a task
  spawned with no bound current take steps under none.
  A flow stage ([CONC-14](tasks.md#conc-14-flows)) copies the owner's remaining steps at
  `flow.stage`. Each item uses a fresh bound of `k` within that copy. The copy refuses
  entry when fewer than `k` remain. The stage state is an input value of this bound.
- **Slices.** While a step bound is current, every 65,536th step a task takes under it is a
  **slice end**: an event ([CONC-10](tasks.md#conc-10-event-order)) at which the scheduler may
  run another task. A flow stage has no event at its slice end. A task's slice end observes
  no cancellation and changes nothing else, so code under a
  bound that computes for long does not keep the tasks it does not wait for from running.
- **Determinism.** A step bound is a fuel and termination bound. Which steps a run takes,
  where a bound stops it and where its slices end depend only on its build, its source and
  the values at its replay point, the scheduler's choices among them, so every run of one build stops at the same step
  ([SEM-8](semantics.md#sem-8-determinism)). They are not the same across builds: an engine
  or optimisation level may take fewer or more steps than the reference count, for example
  by inlining a call or merging loop heads, so no program or test may rely on an exact count
  across engines or optimisation levels. Every build takes at least one step for each
  iteration of a loop in user code and each call into user code that recursion reaches, and
  the extra steps of
  `secret.scram_sha256`, `argon2.id`, `secret.derive_argon2id`, the work steps,
  `host.udp_recv` and `host.udp_send` as the reference count does, so code that computes without end exhausts every bound and work an untrusted input
  chooses is bounded by it. A step taken while no bound is current uses nothing and
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
```hirsel
fn grow(mut xs: List[u64], k: u64) -> Result[(), OutOfTime] {
  let kept: List[u64] = clone(xs)
  let work = replace(mut xs, take kept)  // xs keeps a copy; work is moved into the scope
  let grown = bound steps 100 {
    let mut w = take work
    for i in 0u64..k { list.push(mut w, i) }
    w
  }?                                     // a stop drops w: xs is as it was at entry
  xs = grown
  Ok(())
}
```

### MEM-34: Constant budgets

A computed constant ([LANG-20](language.md#lang-20-constants)) is evaluated under a **constant
budget**, the same for every constant and every build: its own root
step bound of **16,777,216** steps (2^24), its own root limit of **67,108,864** bytes (64 MiB)
and at most **4,096** activations. Each assertion (`const _`) has a budget of its own, as
every other computed constant has. The budget is part of the meaning of the program
([PROG-6](programs.md#prog-6-one-meaning)), so it is counted the same way on every build:

- **Steps**<!-- doc-ref: planned --> are the reference count (MEM-30), not a build's own count. An engine that counts
  otherwise evaluates a constant as if it took the reference count.
- **Bytes** are the logical charges (MEM-24), exactly. MEM-28's latitude does not apply: an
  allocation the evaluating engine elides is charged as if made, and a scratch block a core
  function takes is charged at its logical charge.
- **Activations** are the calls entered and not yet returned, of a function, a callable or a
  core function, with the initialiser counting as one, and the user code entered without a
  call (MEM-30's list) while it runs. Entering the initialiser takes one step, as a call
  does.

`bound steps` and `bound bytes` inside the initialiser reserve from the constant's root
bounds, as anywhere else (MEM-25, MEM-30). They can narrow the budget but never widen it.
The constant is refused, and its run ends, at the first of these:
- a step past what the root step bound has left;
- a reservation from the root step bound larger than what it has left, at a `bound steps`
  entry, which elsewhere returns `Error(OutOfTime(steps: n))`;
- an admission or reservation against the root limit larger than what it has left, which
  elsewhere raises `Capacity` or returns it as a value: a `bound bytes` entry, an
  `isolate`'s entry, a resource literal or a `try_` operation (MEM-21);
- an entry that would make a 4,097th activation.

**The package budget.** A package's computed constants and assertions also share one
**package comptime budget** of **33,554,432** steps (2^25), twice one constant's. Each
constant whose run returns spends the steps that run took, counted as above, the entry
included; a constant another one reads runs once, and its reader spends only its own steps,
and a constant of a dependency is its own package's. Taken in the order the package declares
them (module by module as its index lists them, each module's constants in source order),
the first constant whose steps bring the total past the budget is refused, with `OutOfTime`
as its cause, the same refusal as a step past its own root bound; each constant before it
keeps its value. A constant whose steps depend on the target is counted per target, and the
refusal names the first target past the budget when the constant it reaches differs between
targets. A constant refused for any other cause spends nothing, since its package is refused
already. The package budget, like the per-constant one, sets no value, so raising it turns
only refusals into acceptances. Both budgets apply: no constant runs past its own 2^24
steps, whatever its package has left.

The refusal names `OutOfTime`, `Capacity` or the activation budget as its cause. It is not
a failure of the run (FAIL-2): no scope and no `isolate` in the initialiser sees it, and no
`Result` it returns holds it. A refusal by a bound the initialiser made itself, inside the
root, is ordinary: its value or failure is the one MEM-25 or MEM-30 says, because its size is
written in the program, not set by this rule. So no constant's value depends on the
budget's numbers, and raising them turns only refusals into acceptances.

The numbers are for a constant evaluated once per build, not for a hot path. A table a
program builds once in its initialiser, such as CRC tables, a compiled regular expression,
or a lookup array of 64 KiB, fits with room to spare. Work that is too much for these
numbers belongs at run time, under the run's own bounds.

**The size of a static value.** A static computed constant (a `String` or `FrozenBytes`,
LANG-20) returns bytes that its run admitted under the root limit, so its value is at most
67,108,864 bytes less what else the run holds when it returns. That is its only size limit.
Once stored, the value is static: a read admits only what the literal with the same bytes
admits ([core](core.md)), and the run's charge for building it ends with the run.

```hirsel
fn count(n: u64) -> u64 { let mut i: u64 = 0; while i < n { i += 1 }; i }
const SMALL: u64 = count(1000)            // 1000, in 1,002 steps: the entry, the call and 1,000 back edges
const HUGE: u64 = count(16777216)         // refused: LANG-20, OutOfTime: 2^24 back edges and two steps more
```

Version note (2026-11-25, comptime-phase1-spec-2686; COMPTIME-DIRECTION 3.1 C4 and §5;
built in `PureInput::constant`, `crates/hirsel-core-eval/src/pure.rs`): the new rule. The numbers come from Phase 0's measurements, since nothing else
had been measured:
- **Steps 16,777,216.** Measured by the reference count: `crc32.table()` takes 5,889 steps,
  and `regex.valid` of the 25-byte date pattern takes 80,853 (comptime-eval-split-2612 and
  comptime-e3c-dispatch-2663). That leaves room of 2,849 times for crc and 207 times for
  regex. The old CT-5 budget of 1,000,000 source events, against which the old flate tables
  took 598,608, is a different unit, and 2^24 is 28 times that. At the limit, if each step
  costs as those two workloads do, the tree walker costs about 2,892 to 3,143 host
  instructions per step and the flat engine 1,219 to 1,253, so 48.5 to 52.7 billion and
  20.5 to 21.0 billion instructions. That is seconds, not minutes, for the one constant a
  build might have that large. A step that is a core call can cost more, and the byte limit
  bounds what it works on (MEM-30, "Waits, bytes and tasks").
- **Bytes 67,108,864.** Not measured: Phase 0's probes recorded no bytes. Derived from the
  spec instead. `crc32.table()` charges 0, because an `Inline` array is a plain value.
  `regex.new` of a pattern of length L admits two arenas of 64 × (L + 1) bytes, 1,664 each
  for the date pattern, a group-name list and a class-range pool, which grow with the
  pattern, and a program of at most RE-8's `SIZE_LIMIT`, 10,485,760. A program at
  that limit leaves 56,623,104 bytes, and the arenas alone fit in it for every L up to
  442,367 ((67,108,864 − 10,485,760) / 128 = 442,368 = L + 1), so every pattern a program
  would write as a literal fits with room for the rest. A 64 KiB table, the largest Phase 4 plans, is 1/1,024 of it.
- **Activations 4,096.** Not measured: Phase 0's probes recorded no depth. Derived instead:
  the deepest recursive path in the corpus is 27 activations, so this is 151 times as many.
  `std`'s regex checks its 250 levels of nesting with an explicit work list, not recursion.
  At that path's 27,520 host stack bytes, about 1,019 per activation, 4,096 activations take
  about 4.2 MB, far less than the 128 MiB root stack. So the activation count, not the host
  stack, decides where deep recursion stops, and it stops at the same place on every host.
Reversible before 1.0 in one direction only: raising a number turns refusals into
acceptances and changes no accepted constant's value, while lowering one can refuse
programs that were accepted.

Version note (2026-11-27, capability-reachability-spec-3207; containment study 3202, H4): the
package budget, built in `constant_refusals`, `crates/hirsel-core-driver/src/comptime.rs`.
Before it, each constant had only its own budget, so a package's comptime work grew with its
number of constants: ten constants of 5,000,000 steps each took 8.8 s to check, and were
accepted. **2^25** is twice one constant's budget, so one constant may spend all of its own
and the package still has as much again. Measured by the reference count with `hirsel check`
on every package in `core/programs/` and the `std` they check against, the largest package
total is `std`'s, 11,828 steps over 11 constants, about 1/2,800 of the budget; no program
package passes 400. At the instruction rates above, the budget costs 41 to 105 billion host
instructions at most per package. Reversible in the same one direction.
