# Failure

### FAIL-1: Errors, failures and run stops

An **error** is an `Error(e)` value; it is handled by ordinary code. A **failure** is the
abandonment of a task's computation with a cause (FAIL-2); it runs cleanup (FAIL-8) and is
observed only by the task's joiner (FAIL-14). A **run stop** ends the whole run without
cleanup (FAIL-15). No construct converts one into another, except that `join` delivers a
child's failure to its parent as an error value.

### FAIL-2: Causes

A failure has a primary cause from this closed list, and a site, the operation that
raised it:

| cause | raised by |
|---|---|
| `Overflow` | checked integer arithmetic, conversion and shift counts ([SEM-1](semantics.md#sem-1-integers)) |
| `Bounds` | indexing and slicing out of range; `channel.new(0)` |
| `Division` | integer division or remainder by zero |
| `Unwrap` | `option.unwrap` of `None`, `result.unwrap` of `Error` |
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
fn put(mut q: List[File], take f: File) -> Result[(), (Capacity, File)] {
  list.push(mut q, take f)              // on Capacity the error owns f
}
```

### FAIL-4: Constructor temporaries

A record or variant construction evaluates its field expressions left to right into
statement temporaries; a placed literal `T(..) in r` and a resource literal then admit their
charge. If a field expression or the admission fails, the temporaries are dropped by the
statement's cleanup in reverse evaluation order, their finalizers included, and the error is
bare `Capacity` ([T1.1, T1.2](ownership.md#own-26-the-transition-table)). A core constructor
that takes an owner, such as `box.new`, returns it in the error instead (T1.3).

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

Bytes that a failed `span.push`, a failed or unused sized destination, or an abandoned
span-builder allocation took in a region stay charged to the region until it closes
([MEM-8](memory.md#mem-8-close)). Nothing refunds them earlier.

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
Cleaning a value runs its finalizer if it has one, then cleans its fields in reverse
declaration order and the elements of a sequence from the highest index down, then frees
and refunds its block. A region opened by an exited block closes after its block's owners
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
task has one primary cell from its start, and each resource value has one, admitted with
the resource ([MEM-24](budgets.md#mem-24-logical-charges)); a resource created by a
finalizer takes its cell and storage from the finalizer's grant
([MEM-27](budgets.md#mem-27-finalizer-grants)). A cause raised by a finalizer goes into its
resource's cell, and any other cause into the task's primary cell. If a finalizer cannot
admit an obligation while a failure is active, its grant being exhausted, that is its own
failure with cause `Capacity`, recorded in its resource's cell, and cleanup continues. So
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
([MEM-24](budgets.md#mem-24-logical-charges)) and has type `Result[T, Capacity]`. A
resource is cleaned exactly once: by its finalizer on implicit drop, or by an explicit
consuming function. A consuming function in the defining module ends a resource without
its finalizer by a whole destructure ([T3.7](ownership.md#own-26-the-transition-table)),
which is how fallible finalisation (`close`, `commit`, `flush`) is written; the
destructure refunds the resource's unused cause cell and grant after its fields are
transferred. There is no `forget`. A finalizer has a unit result and no `?` of its own; the
functions it calls may use `?` inside them. It is shielded: it observes no cancellation
([CONC-7](tasks.md#conc-7-cancellation-points)), though it may call `waits` operations, and
it may spawn from its grant. It uses only the capabilities stored in the resource and its grant
([MEM-27](budgets.md#mem-27-finalizer-grants)); it cannot move, copy or return the
resource.

```hirsel
resource type File { handle: HostFile }
  drop(f) { let _ = host.unlock(mut f.handle) }
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

In v1 a failure ends the task in which it happens, after the task's cleanup. Its joiner
receives `Failed(causes)`, or `Cancelled(causes)` when the primary cause is `Cancelled`, as
an error value of `join` ([CONC-6](tasks.md#conc-6-join)). A failure in `main` ends the run
with the failure as its outcome. No v1 construct inside a task catches a failure, and no v1
code may rely on a failure ending its task: a later isolating scope (R8 in
[reserved](reserved.md)) may resume after that scope's cleanup.

### FAIL-15: Run stops

Exhausting the stack, a counter that would pass its 64-bit range (a counted value's holders,
[MEM-16](memory.md#mem-16-counting); a channel's senders, [CONC-4](tasks.md#conc-4-channels)),
the engine failing to obtain memory, and termination by the host are run stops. A run stop
ends every task without cleanup. It is reported as the run's outcome and is not a failure:
no cause chain is formed and no joiner observes it.
