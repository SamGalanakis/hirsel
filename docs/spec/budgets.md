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
task's result. So the lexical end of a limit and the death of what it funded are separate,
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
engine's layout: an engine may hold fewer bytes (MEM-28).

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
price is a `fn` or a `let`, never a `const` (LANG-20 calls no function). Their results are
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
  `?`, patterns and `match` run no user code and take none. A core function's call, and a
  foreign function's ([FFI-7](foreign.md#ffi-7-host-stack-and-waits)), is one
  step plus any steps taken by the user code it enters. `secret.scram_sha256` takes
  one further step for each PBKDF2 iteration it admits, at the call and before any
  derivation, because an untrusted server chooses the work ([TLS-6](tls.md#tls-6-scram-sha-256)).
  `argon2.id` and `secret.derive_argon2id` take one further step for each Argon2 block
  compression their parameters choose, at the call and before any work, because a stored
  hash or a caller names a cost that no limit bounds ([core](core.md#argon2id)).
  `crc32c.update` and `crc64nvme.update` take one further step before each nonempty
  16 KiB chunk, including a final partial chunk. `host.udp_recv` takes one further step for each 64 entries, or part of 64, of
  its `out` spare capacity, and one for each 16 KiB, or part, of the most it can deliver
  (`min(r, e × (slot_bytes − 256))`). `host.udp_send` takes one for each 64 packets, or part
  of 64, of its `pkts`, and after validating them one for each 16 KiB, or part, of the sum of
  their lengths. The product and the sum are exact (128-bit) before rounding, so neither
  wraps. All are taken at the call and before issue
  ([NET-16](net.md#net-16-udp-sockets-and-batches)). Code an engine adds for drop, clone and growth
  takes no steps. This is the **reference count**, the count the reference evaluator takes;
  a build may count otherwise (Determinism, below).
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
  nothing. Because a core call is one step, a step bound bounds time only together with the
  limits that bound the sizes core calls work on.
  A task spawned while a bound is current runs under its own root bound of what that bound has left at the
  spawn; the spawner's bound is not reduced, and no steps return. No two tasks share a bound, so where a
  bound stops a task follows from that task's build, source and values alone. The steps a run takes under a bound
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
- **Determinism.** A step bound is a fuel and termination bound. Which steps a run takes,
  where a bound stops it and where its slices end depend only on its build, its source and
  the values at its replay point, so every run of one build stops at the same step
  ([SEM-8](semantics.md#sem-8-determinism)). They are not the same across builds: an engine
  or optimisation level may take fewer or more steps than the reference count, for example
  by inlining a call or merging loop heads, so no program or test may rely on an exact count
  across engines or optimisation levels. Every build takes at least one step for each
  iteration of a loop in user code and each call into user code that recursion reaches, and
  the extra steps of
  `secret.scram_sha256`, `argon2.id`, `secret.derive_argon2id`, `crc32c.update` and
  `crc64nvme.update`, `host.udp_recv` and `host.udp_send` as the reference count does, so code that computes without end exhausts every bound and work an untrusted input
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
