# Failure

### FAIL-1: Errors, failures and run stops

An **error** is an `Error(e)` value; it is handled by ordinary code. A **failure** is the
abandonment of a task's computation with a cause (FAIL-2); it runs cleanup (FAIL-8) and is
observed only by the task's joiner (FAIL-14) or by an enclosing `isolate` (FAIL-16). A **run stop** ends the whole run without
cleanup (FAIL-15). No construct converts one into another, except that `join` delivers a
child's failure to its parent as an error value, and `isolate` delivers a failure in its
body as a value (FAIL-16).

### FAIL-2: Causes

A failure has a primary cause from this closed list, and a site, the operation that
raised it:

| cause | raised by |
|---|---|
| `Overflow` | checked integer arithmetic, conversion and shift counts ([SEM-1](semantics.md#sem-1-integers)) |
| `Bounds` | indexing and slicing out of range; `channel.new(0)` |
| `Division` | integer division or remainder by zero |
| `Unwrap` | [`option.unwrap`](../../core/core/option.hirsel) of `None`, [`result.unwrap`](../../core/core/result.hirsel) of `Error` ([core](core.md#contracts-the-signatures-do-not-state)) |
| `Fail(text)` | `fail(msg)`, which has type `Never` |
| `Cancelled` | a cancellation point that observes cancellation ([CONC-7](tasks.md#conc-7-cancellation-points)) |
| `Capacity` | an allocation beyond a finalizer's grant ([MEM-27](budgets.md#mem-27-finalizer-grants)) |

`Capacity` outside a finalizer is an error, never a failure.

```hirsel
fn mean(xs: List[i64]) -> i64 { list.sum(xs) / int.narrow(list.len(xs)) }   // Division when xs is empty
```

### FAIL-3: Commit points

Every operation that takes custody of an input has a **commit point**: the step at which
custody passes. It is entry for a call ([T2.3](ownership.md#own-26-the-transition-table)),
publication for a send ([CONC-4](tasks.md#conc-4-channels)) and admission for an allocating
constructor or core operation ([MEM-21](budgets.md#mem-21-admission)). Before it, the
operation consumes nothing: inputs that its error type lists come back in the error, and
every other input stays where it was. After it, the operation may still fail (a replaced
value's finalizer, a callee's own failure), and what it did is not undone: a callee's
effects on its `mut` arguments stand, and so does an installed value.

```hirsel
fn put(mut q: List[HostFile], take f: HostFile) -> Result[(), (Capacity, HostFile)] {
  list.push(mut q, take f)              // on Capacity the error owns f
}
```

### FAIL-4: Constructor temporaries

A record or variant construction evaluates its field expressions left to right into
statement temporaries; a placed literal `T(..) in r` and a resource literal then admit their
charge. If a field expression fails, the completed temporaries are dropped by the
statement's cleanup in reverse evaluation order, their finalizers included
([T1.1](ownership.md#own-26-the-transition-table)). If the admission fails, nothing is
cleaned and no finalizer runs: a resource literal hands its fields back in its error, as
`box.new` hands back its value (T1.3), and a placed literal's error is bare `Capacity`
([T1.2](ownership.md#own-26-the-transition-table)).

```hirsel
type Pair { a: HostFile, b: HostFile }
fn pair(d: Dir) -> Result[Pair, IoError] waits {
  Ok(Pair(a: host.open(d, "a", Read)?, b: host.open(d, "b", Read)?))   // if b fails, a is dropped once
}
```

### FAIL-5: Multi-step operations

An operation that allocates in several steps, such as `clone`, `clone_in` and map
insertion that grows, undoes its own partial results before it returns its error, as
[T1.5](ownership.md#own-26-the-transition-table) states. A callback's effects inside such an
operation are never undone.

### FAIL-6: Region prefixes

Bytes that a failed `span.push`, a failed or unused sized destination or writer, or an
abandoned span-builder allocation took in a region stay charged to the region until it
closes ([MEM-8](memory.md#mem-8-close)). Nothing refunds them earlier, and nothing reuses
them; only `finish_trim` of a completed builder refunds its unused end
([MEM-4](memory.md#mem-4-sized-destinations)).

### FAIL-7: One owner per survivor

After any error or failure, each surviving value has exactly one owner: the error value,
its original slot, or a statement temporary that the current cleanup will drop. The
[transition table](ownership.md#own-26-the-transition-table) names that owner for every
operation; an operation not in the table follows FAIL-3.

### FAIL-8: Exit-edge cleanup

On every exit edge of a scope (block end, `break`, `continue`, `return`, `?`, failure and
cancellation), the views of the exited scopes end first, and views of scopes not exited
stay; then owning slots are cleaned, innermost scope first and each scope's slots in reverse
declaration order (the slots a pattern binds are declared in pattern order). A live slot is
cleaned, a maybe slot is cleaned when its flag is set, and a moved slot is not touched.
Cleaning a value runs its finalizer if it has one, then cleans its record and tuple fields
in reverse declaration order, the elements of a complete `List` or `Inline` array from
index 0 up, a `Map`'s entries as [core](core.md) says and a queue's items as
[CONC-9](tasks.md#conc-9-queue-drop) says, then frees and refunds its block. A construction
that fails cleans its completed temporaries in reverse evaluation order
([T1.1](ownership.md#own-26-the-transition-table)). A region opened by an exited block closes after its block's owners
are cleaned.

```hirsel
fn two(d: Dir) -> Result[(), IoError] waits {
  let a = host.open(d, "a", Read)?
  let mut b = host.open(d, "b", Append)?
  host.try_lock(mut b)?                // on error: b dropped, then a
  Ok(())
}
```

### FAIL-9: Pending results

An exit edge that carries a value (`return v`, `break 'l v`, a block's final expression)
first stages the value as the edge's **pending result**, then cleans the exited scopes. If
a drop on the edge fails, the failing value's fields are cleaned (FAIL-12), then the
pending result is dropped, then the remaining drops of the exited scopes run, each cause
appended in the order it happens, and the edge becomes failure propagation past its original target,
whose continuation never runs ([T6.3](ownership.md#own-26-the-transition-table)).

### FAIL-10: Cause chain

A failure keeps its primary cause. A further failure during cleanup (a finalizer failing,
FAIL-12) appends its cause to the chain in the order it happens, and cleanup continues with
the next value. The chain is delivered whole to the joiner (FAIL-14). Cleaning never
replaces the primary cause, and no cause is dropped.

A cause is stored in a **cause cell**. Propagating a failure moves the existing chain and
reserves nothing. A cell is reserved only with an obligation that can add a cause: each
task has one primary cell from its start, and each resource value that is not mechanical
([MEM-24](budgets.md#mem-24-logical-charges), FAIL-13) has one, admitted with
the resource ([MEM-24](budgets.md#mem-24-logical-charges)); a resource created by a
finalizer takes its cell and storage from the finalizer's grant
([MEM-27](budgets.md#mem-27-finalizer-grants)). A cause raised by a finalizer goes into its
resource's cell, and any other cause into the task's primary cell. If a finalizer cannot
admit an obligation while a failure is active, its grant being exhausted, that is its own
failure with cause `Capacity`, recorded in its resource's cell, and cleanup continues; an
`isolate`'s refused cell is a value instead ([FAIL-16](#fail-16-isolation)). So
code that makes no resources performs no cause admission beyond task startup, the chain has no depth limit and
is never truncated, and recording, linking and delivering causes never admit. An unused
cell is refunded with its obligation; a used cell stays with the chain, through `join`, until
the `Causes` value that holds it is cleaned, and is then refunded to the limit that admitted
it. A `Fail` cause takes ownership of its message.

### FAIL-11: Resources

A `resource type` declares a finalizer after its fields:

```hirsel
resource type Temp { dir: Dir, name: String }
  drop(t) { let _ = host.remove(t.dir, t.name) }
```

A resource literal admits the resource's storage, cause cell and grant
([MEM-24](budgets.md#mem-24-logical-charges)) and has type `Result[T, (Capacity, F)]`, with
`F` its one field or the tuple of its fields in declaration order, or `Result[T, Capacity]`
when it has no fields ([T1.2](ownership.md#own-26-the-transition-table)). A
resource is cleaned exactly once: by its finalizer on implicit drop, or by an explicit
consuming function. A consuming function in the defining module ends a resource without
its finalizer by a whole destructure ([T3.7](ownership.md#own-26-the-transition-table)),
which is how fallible finalisation (`close`, `commit`, `flush`) is written; after its fields
are transferred the destructure frees and refunds the resource's whole block (its field
storage, unused cause cell and unused grant), and the blocks its fields own stay charged
with them. There is no `forget`. A finalizer has a unit result and no `?` of its own; the
functions it calls may use `?` inside them. It is shielded: it observes no cancellation
([CONC-7](tasks.md#conc-7-cancellation-points)), though it may call `waits` operations, and
it may spawn from its grant. It uses only the capabilities stored in the resource and its grant
([MEM-27](budgets.md#mem-27-finalizer-grants)); it cannot move, copy or return the
resource.

```hirsel
resource type File { handle: HostFile }
  drop(f) { let _ = host.unlock(mut f.handle) }
pub fn open(take h: HostFile) -> Result[File, (Capacity, HostFile)] { File(handle: take h) }
pub fn close(take f: File) -> Result[(), IoError] waits {
  let File(handle: h) = take f          // f ends without its finalizer
  host.close(take h)
}
```
```hirsel
resource type T { f: HostFile }
  drop(t) { host.unlock(mut t.f)? }     // refused: FAIL-11, ? in a finalizer
```

### FAIL-12: Finalizer failure

A failure inside a finalizer abandons the finalizer body, still cleans the resource's
fields, and is a failed drop: on a normal exit edge it starts propagation (FAIL-9); during
failure cleanup it appends to the cause chain (FAIL-10). In both cases cleanup continues
with the next value.

### FAIL-13: Mechanical release

Ending views, freeing and refunding own blocks, closing regions, releasing counted nodes
and failure propagation itself never allocate, never admit, never run user code other than
finalizers and never fail.

### FAIL-14: Failure boundary

A failure that no `isolate` catches (FAIL-16) ends the task in which it happens, after the task's cleanup. Its joiner
receives `Failed(causes)`, or `Cancelled(causes)` when the primary cause is `Cancelled`, as
an error value of `join` ([CONC-6](tasks.md#conc-6-join)). A failure in `main` ends the run
with the failure as its outcome. No construct other than `isolate` catches a failure.

### FAIL-15: Run stops

Exhausting the stack, a counter that would pass its 64-bit range (a counted value's holders,
[MEM-16](memory.md#mem-16-counting); a channel's senders, [CONC-4](tasks.md#conc-4-channels);
one the engine keeps itself), the host refusing memory or a service the engine needs, and
termination by the host are run stops. A run stop ends every task without cleanup. It is
reported as the run's outcome and is not a failure: no cause chain is formed, no cleanup or
finalizer runs, and no joiner observes it. In a process it exits with status 102 and
writes one line to stderr, `run stop: ` and the stop's name
([PROG-3](programs.md#prog-3-entry)):

| stop | name |
|---|---|
| the stack is exhausted | `Stack` |
| a counted value's holders would pass `2^64 − 1` | `CountOverflow` |
| a channel's senders would pass `2^64 − 1` | `ChannelSenders` |
| an engine counter would pass `2^64 − 1`: task ids, host handles, temporary names, runs in the process, destination identities ([MEM-4](memory.md#mem-4-sized-destinations)) | `TaskIds`, `HostHandles`, `HostNames`, `ActiveRuns`, `DestinationIds` |
| the host refuses memory the engine needs | `HostMemory` |
| the host refuses the signal stack, a signal action, the clock or the page size | `SignalStack`, `Signal`, `Clock`, `PageSize` |

Termination by the host writes nothing and its status is the host's.

### FAIL-16: Isolation

`isolate(body)`, with `body: scoped fn() -> R`, runs `body` once and returns a failure in
it as a value, `Result[Result[R, Causes], Capacity]`, instead of ending the task. It is a
recovery boundary and undoes nothing:

- It first reserves a new cause cell, charged as a task's
  ([MEM-24](budgets.md#mem-24-logical-charges)) and admitted from the current limit, which
  inside a finalizer is its grant ([MEM-27](budgets.md#mem-27-finalizer-grants)). The cell
  stands in for the **current obligation**'s cell ([FAIL-10](#fail-10-cause-chain)): the
  cell of the resource whose finalizer is running, the innermost one, or else the task's
  primary cell. When that admission fails, under a grant too, `isolate` returns the value
  `Error(c)`, `c` the refused `Capacity`, and never fails; it does not call `body`, and the
  chain is as it was. Otherwise it sets aside the chain active at entry, a failure being
  cleaned when a finalizer runs on a failure path, and calls `body` with the activation of
  a call that receives it ([OWN-21](ownership.md#own-21-scoped-callbacks)).
- If `body` returns `r`, the reserved cell is refunded, the chain set aside is restored, and
  `isolate` returns `Ok(Ok(r))`.
- If `body` fails, the failure is cleaned as on every exit edge (FAIL-8) up to `isolate`:
  the owners, loans, captures and pending results of `body` are cleaned, and every cause
  that cleanup adds joins the chain (FAIL-10). `isolate` then returns `Ok(Error(causes))`
  holding every cause raised since its entry, and the chain set aside is restored as it
  was. When the caught failure used the current obligation's cell, the reserved cell takes
  its place, so the code after `isolate` has an unused cell to fail into; when it did not,
  every caught cause having gone into resources' cells, the reserved cell is refunded. A
  failure is caught whatever its primary cause other than `Cancelled`: `Overflow`,
  `Bounds`, `Division`, `Unwrap`, `Fail` or a finalizer's `Capacity`
  ([MEM-27](budgets.md#mem-27-finalizer-grants)).
- A failure whose primary cause is `Cancelled` is not caught: after the same cleanup the
  reserved cell is refunded, the chain set aside is restored with the failure's causes
  after it, and the failure goes on past `isolate`. A run stop (FAIL-15)
  is not caught, and nothing resumes at the operation that failed. `Capacity` outside a
  finalizer is an error that `body` handles as a value (FAIL-2), never a failure.

`body`'s type is not `waits`, so nothing in it waits ([LANG-8](language.md#lang-8-signatures)),
and it takes no parameter, so it has no `mut` or `take` parameter; a callable that does
not fit that type is refused by [LANG-13](language.md#lang-13-callables). `R` is a type
parameter and therefore never scoped ([OWN-19](ownership.md#own-19-scoped-values)).

`isolate` is refused here, at its argument, when the argument's capture descriptor
(OWN-21) has a `mut` entry. The descriptor checked is the transitive one: it unites the
descriptors of the scoped callbacks the body invokes or passes on, with `mut` dominating
view, and a scoped callback that the enclosing function received as a parameter counts as
a `mut` entry, because its descriptor is its caller's and is not known in this body. A
capture the body lends `mut`, a `Box` payload reached through it included, is a `mut`
entry (LANG-14). An `own fn` or `fn` value lends nothing outside its environment, which
its calls only view ([LANG-14](language.md#lang-14-captures)), so the body may call one.

So if `body` fails, every place outside it that it reached through a view holds the value
that was installed in it at entry. Effects that do not install into such a place persist,
and are not undone. They include appends to a log
([MEM-29](memory.md#mem-29-append-only-logs)), sends on a channel and the queue changes
they make ([CONC-4](tasks.md#conc-4-channels)), provider operations already issued
([CAP-5](capabilities.md#cap-5-provider-operations)), counted holders' counts
([MEM-16](memory.md#mem-16-counting)) and bytes allocated in an outer region, which stay
charged until it closes (FAIL-6). A program that needs all or nothing keeps its working
state in owned values made inside `body`, returns them on success and installs them after
`isolate` returns. A single-`Result` form and a form that moves owned state into the body
and back are reserved ([reserved](reserved.md), F).

```hirsel
fn run(db: Map[u64, u64], step: own fn(Map[u64, u64], mut Map[u64, u64])) -> Result[Result[Map[u64, u64], Causes], Capacity] {
  isolate(fn() -> Map[u64, u64] {
    let mut writes: Map[u64, u64] = map.new()
    step(db, mut writes)                          // a failure here cleans writes; db is unchanged
    writes
  })
}
```
```hirsel
fn report(tx: Sender[u64], xs: List[u64]) -> u64 {
  match isolate(fn() -> u64 {
    let _ = channel.try_send(tx, 1)               // stays sent when the sum fails
    list.sum(xs)
  }) {
    Ok(Ok(s)) => s,
    Ok(Error(_)) => 0,
    Error(_) => 0,
  }
}
```
```hirsel
fn bump(mut n: u64) -> u64 {
  let _ = isolate(fn() -> u64 { n = n + 1; n })   // refused: FAIL-16, n is captured mut
  n
}
```
```hirsel
fn count(xs: List[u64]) -> u64 {
  let mut seen: u64 = 0
  let tick = scoped fn() { seen = seen + 1 }
  let _ = isolate(fn() -> u64 { tick(); list.len(xs) })   // refused: FAIL-16, tick lends seen mut
  seen
}
```
```hirsel
fn guard(f: scoped fn() -> u64) -> u64 {
  match isolate(fn() -> u64 { f() }) {            // refused: FAIL-16, f's descriptor is not known here
    Ok(Ok(v)) => v,
    _ => 0,
  }
}
```
